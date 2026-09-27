use super::*;

fn landing_details(html: &str) -> Option<(u64, String)> {
    let document = scraper::Html::parse_document(html);
    let body = document.select(&scraper::Selector::parse(".tier-body").ok()?).find(|body| body.select(&scraper::Selector::parse(".tier-name").unwrap()).next().is_some())?;
    let name = body.select(&scraper::Selector::parse(".tier-name").ok()?).next()?.text().collect::<String>();
    let size = body.select(&scraper::Selector::parse(".tier-feat").ok()?).next()?.text().collect::<String>();
    let capture = regex::Regex::new(r"(?i)^\s*([0-9]+(?:[.,][0-9]+)?)\s*(B|KB|MB|GB|TB)\s*$").ok()?.captures(&size)?;
    let number: f64 = capture[1].replace(',', ".").parse().ok()?;
    let multiplier = match capture[2].to_ascii_uppercase().as_str() { "KB" => 1024f64, "MB" => 1024f64.powi(2), "GB" => 1024f64.powi(3), "TB" => 1024f64.powi(4), _ => 1. };
    Some(((number * multiplier).round() as u64, name.trim().into()))
}

pub(super) async fn enrich(http: &Client, packages: Vec<Package>) -> Vec<Package> {
    use futures_util::stream;
    stream::iter(packages.into_iter().enumerate().map(|(index, mut package)| async move {
        if index < 64 && package.expected_size.is_none() && Url::parse(&package.url).is_ok_and(|u| u.scheme() == "https" && matches!(u.host_str(), Some("1fichier.com" | "www.1fichier.com")) && u.username().is_empty() && u.password().is_none()) {
            let probe = async {
                let mut response = http.get(&package.url).timeout(Duration::from_secs(8)).send().await.ok()?;
                if !response.status().is_success() || !response.headers().get(reqwest::header::CONTENT_TYPE)?.to_str().ok()?.contains("text/html") { return None; }
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.ok()? { if bytes.len() + chunk.len() > 256 * 1024 { return None; } bytes.extend_from_slice(&chunk); }
                landing_details(&String::from_utf8_lossy(&bytes))
            };
            if let Some((bytes, name)) = probe.await {
                package.expected_size = Some(bytes);
                if package.archive_file_name.is_none() {
                    package.archive_format_hint = Some(if name.to_ascii_lowercase().ends_with(".zip") { "zip" } else { "rar" }.into());
                    package.archive_file_name = Some(name);
                }
            }
        }
        (index, package)
    })).buffer_unordered(4).collect::<Vec<_>>().await.into_iter().collect::<std::collections::BTreeMap<_, _>>().into_values().collect()
}

#[tauri::command]
pub(super) async fn inspect_package_sizes(state: State<'_, AppState>, packages: Vec<Package>) -> Result<Vec<Package>, String> {
    if packages.len() > 256 { return Err("Too many package links".into()); }
    Ok(enrich(&state.http, packages).await)
}

#[tauri::command]
pub(super) async fn refresh_job_details(app: AppHandle, job_id: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let Some(saved) = job_store::record(&app, &job_id) else { return Ok(()); };
    let Some(mut request) = saved.request else { return Ok(()); };
    if saved.progress.removed { return Ok(()); }
    let mut primary = vec![request.package.clone()]; primary.extend(request.archive_parts.clone());
    let mut enriched = enrich(&state.http, primary).await.into_iter(); request.package = enriched.next().unwrap(); request.archive_parts = enriched.collect();
    if let Some(backport) = &mut request.backport {
        let mut parts = vec![backport.package.clone()]; parts.extend(backport.parts.clone());
        let mut enriched = enrich(&state.http, parts).await.into_iter(); backport.package = enriched.next().unwrap(); backport.parts = enriched.collect();
    }
    let components = {
        let mut store = state.retry.lock().unwrap();
        let Some(record) = store.records.get_mut(&job_id).filter(|r| !r.progress.removed) else { return Ok(()); };
        let components = job_store::request_components(&request, &record.downloads);
        record.request = Some(request); record.progress.components = components.clone(); store.save(&job_id)?; components
    };
    let current = state.jobs.lock().unwrap().get(&job_id).cloned();
    if let Some(mut progress) = current { progress.components = components; emit(&app, progress); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_file_card_size_ignores_advertising_numbers() {
        let html = r#"<p>Unlimited 100 GB</p><div class="tier-body"><span class="tier-name">backport.zip</span><span class="tier-feat">98.49 MB</span></div>"#;
        let (bytes, name) = landing_details(html).unwrap(); assert_eq!(bytes, (98.49f64 * 1024. * 1024.).round() as u64); assert_eq!(name, "backport.zip");
        assert!(landing_details("<p>98 MB</p>").is_none());
    }
}

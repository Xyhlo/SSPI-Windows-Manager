use super::*;
use scraper::ElementRef;

pub(super) fn utf8_window(value: &str, start: usize, end: usize) -> &str {
    let mut start = start.min(value.len()); let mut end = end.min(value.len());
    while !value.is_char_boundary(start) { start += 1; }
    while !value.is_char_boundary(end) { end -= 1; }
    &value[start.min(end)..end]
}

pub(super) fn source_link(link: ElementRef<'_>, page: &str) -> Option<String> {
    let attrs = link.value();
    let mut raw = attrs.attr("href").unwrap_or("").to_owned();
    if let Some(first) = attrs.attr("data-d1") {
        raw = format!("{}{}{}", first, attrs.attr("data-d2").unwrap_or(""), attrs.attr("data-path").unwrap_or(""));
    } else if let Some(domain) = attrs.attr("data-domain") {
        raw = format!("{}{}", domain, attrs.attr("data-path").unwrap_or(""));
    } else if placeholder_href(&raw) {
        let assignment = Regex::new(r#"this\.href\s*=\s*((?:'[^']*'|"[^"]*")(?:\s*\+\s*(?:'[^']*'|"[^"]*"))*)\s*(?:;|$)"#).unwrap();
        let capture = assignment.captures(attrs.attr("onclick").unwrap_or(""))?;
        let literal = Regex::new(r#"'([^']*)'|"([^"]*)""#).unwrap();
        raw = literal.captures_iter(&capture[1]).filter_map(|c| c.get(1).or_else(||c.get(2)).map(|v|v.as_str().to_owned())).collect();
    }
    if placeholder_href(&raw) { return None; }
    let mut url = Url::parse(page).ok()?.join(&raw).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() { return None; }
    url.set_fragment(None);
    Some(url.to_string())
}

fn roots<'a>(doc: &'a Html, step: &Value) -> Vec<ElementRef<'a>> {
    for selector in step_strings(step, "rootSelectors").into_iter().chain(["body".into()]) {
        if let Ok(selector) = Selector::parse(&selector) {
            let found: Vec<_> = doc.select(&selector).collect();
            if !found.is_empty() { return found; }
        }
    }
    vec![doc.root_element()]
}

fn leaf_blocks(root: ElementRef<'_>) -> Vec<ElementRef<'_>> {
    let selector = Selector::parse("p,li,tr,h1,h2,h3,h4,h5,h6").unwrap();
    root.select(&selector).filter(|node| node.select(&selector).next().is_none()).collect()
}

fn last_capture(text: &str, pattern: &str) -> String {
    if pattern.is_empty() { return String::new(); }
    Regex::new(pattern).ok().and_then(|r| r.captures_iter(text).last().and_then(|c| c.get(1).map(|v|v.as_str().to_owned()))).unwrap_or_default()
}

pub(super) fn scoped_titles(docs: &[WorkItem], step: &Value) -> Vec<WorkItem> {
    let id_re = Regex::new(r"(?i)\b(?:CUSA|PPSA|SLUS|SLES|SCUS|SCES|SLPS|SLPM|SCPS|SCAJ|SLAJ|SLKA|SLKS|SCKA)\d{5}\b").unwrap();
    let mut out = Vec::new(); let mut seen = HashSet::new();
    for doc in docs {
        let html = Html::parse_document(&doc.html);
        for root in roots(&html, step) {
            let image = if doc.image.is_empty() {
                root.select(&Selector::parse("img[data-src],img[src]").unwrap()).find_map(|img| {
                    let raw = img.value().attr("data-src").or_else(||img.value().attr("src"))?;
                    let url = Url::parse(&doc.url).ok()?.join(raw).ok()?;
                    matches!(url.scheme(), "http"|"https").then(||url.to_string())
                }).unwrap_or_default()
            } else { doc.image.clone() };
            for block in leaf_blocks(root) {
                let text = collapse(&block.text().collect::<Vec<_>>().join(" "));
                for id in id_re.find_iter(&text) {
                    let id = id.as_str().to_ascii_uppercase();
                    if !seen.insert(id.clone()) { continue; }
                    out.push(WorkItem { title_id: id, region: region_from(&text, &HashMap::new()),
                        image: image.clone(), html: String::new(), ..doc.clone() });
                    if out.len() >= MAX_RESULTS { return out; }
                }
            }
        }
    }
    out
}

pub(super) fn scoped_packages(docs: &[WorkItem], requested: &str, step: &Value) -> Vec<WorkItem> {
    let id_re = Regex::new(r"(?i)\b(?:CUSA|PPSA|SLUS|SLES|SCUS|SCES|SLPS|SLPM|SCPS|SCAJ|SLAJ|SLKA|SLKS|SCKA)\d{5}\b").unwrap();
    let anchor = Selector::parse("a").unwrap();
    let allowed = step_strings(step, "allowedLinkHosts");
    let mut out = Vec::new(); let mut seen = HashSet::new();
    for doc in docs {
        let html = Html::parse_document(&doc.html);
        for root in roots(&html, step) {
            let mut current = WorkItem { kind: "unknown".into(), title_id: String::new(), ..doc.clone() };
            for block in leaf_blocks(root) {
                let text = collapse(&block.text().collect::<Vec<_>>().join(" "));
                if let Some(id) = id_re.find(&text) {
                    if !current.title_id.eq_ignore_ascii_case(id.as_str()) {
                        current = WorkItem { kind: "unknown".into(), title_id: id.as_str().to_ascii_uppercase(),
                            region: region_from(&text, &HashMap::new()), ..doc.clone() };
                    }
                }
                if !current.title_id.eq_ignore_ascii_case(requested) { continue; }
                let password = last_capture(&text, &step_string(step, "passwordPattern"));
                if !password.is_empty() { current.archive_password = password.chars().take(128).collect(); continue; }
                if let Some(rules) = step.get("kindRules").and_then(Value::as_array) {
                    for rule in rules {
                        let pattern = step_string(rule, "pattern");
                        // The original v3 PS4 recipe used one negative lookahead.
                        // Keep its precise base-vs-game-update meaning on Rust regex.
                        let (pattern, game_guard) = if pattern.contains("(?!\\s*update)") {
                            (pattern.replace("(?!\\s*update)", ""), true)
                        } else { (pattern, false) };
                        let matches = Regex::new(&pattern).ok().is_some_and(|r| r.find_iter(&text).any(|m| {
                            !game_guard || !text[m.end()..].trim_start().to_ascii_lowercase().starts_with("update")
                        }));
                        if !matches { continue; }
                        current.kind = step_string(rule,"kind");
                        current.label = text.chars().take(180).collect();
                        current.package_version = last_capture(&text, &step_string(step,"versionPattern"));
                        current.firmware = last_capture(&text, &step_string(step,"firmwarePattern"));
                        if current.firmware.is_empty() { current.firmware = last_capture(&text, r"\((\d{1,2}\.\d{2})\+\)"); }
                        current.group_id = format!("section-{}", &sha256(format!("{}|{}|{}|{}|{}|{}",doc.url,requested,current.kind,current.package_version,current.firmware,current.label).as_bytes())[..20]);
                        break;
                    }
                }
                if current.kind == "unknown" { continue; }
                for link in block.select(&anchor) {
                    let Some(url) = source_link(link, &doc.url) else { continue; };
                    let parsed = Url::parse(&url).unwrap();
                    let host = parsed.host_str().unwrap_or("").trim_start_matches("www.");
                    if !allowed.iter().any(|h| h.eq_ignore_ascii_case(host)) { continue; }
                    if id_re.find(&url).is_some_and(|id| !id.as_str().eq_ignore_ascii_case(requested)) { continue; }
                    if !seen.insert(format!("{}|{url}", current.group_id)) { continue; }
                    out.push(WorkItem {url: url.clone(), html: String::new(), source_page: doc.url.clone(),
                        parent_url: doc.url.clone(), hoster: host.to_owned(), ..current.clone()});
                    if out.len() >= MAX_ITEMS { return out; }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recipe() -> Value { serde_json::from_str(include_str!("../../../../sources/gamesource/recipe.json")).unwrap() }
    #[test]
    fn scoped_region_kind_version_and_password_are_preserved() {
        let r=recipe(); let step=&r["resolveSteps"][7];
        let doc=WorkItem {url:"https://dlpsgame.com/fixture-ps4/".into(), name:"Fixture".into(), html:r#"<div class="entry-content">
        <p>CUSA12345 – USA</p><p>Password: [DLPSGAME.COM]</p><p>Game: <a href="https://1fichier.com/?base">Link</a></p>
        <p>Update v1.42 (9.00+): <a href="https://downloadgameps3.net/update">Link</a></p>
        <p>DLC: <a href="https://1fichier.com/?dlc">Link</a></p><p>CUSA54321 – EUR</p>
        <p>Game: <a href="https://1fichier.com/?eu">Link</a></p></div>"#.into(), ..Default::default()};
        let pkgs=scoped_packages(&[doc.clone()],"CUSA12345",step); assert_eq!(pkgs.len(),3);
        assert_eq!(pkgs[1].kind,"update"); assert_eq!(pkgs[1].package_version,"1.42"); assert_eq!(pkgs[1].firmware,"9.00");
        assert_eq!(pkgs[1].archive_password,"[DLPSGAME.COM]"); assert_eq!(pkgs[2].package_version,"");
        assert_eq!(scoped_titles(&[doc], &r["searchSteps"][7]).len(),2);
    }
    #[test]
    fn ps5_titles_and_literal_links_are_supported_without_javascript() {
        let html=Html::parse_document(r##"<p>PPSA12345 – Europe</p><a href="#" data-d1="https://1fic" data-d2="hier.com" data-path="/?abc">One</a><a href="#" onclick="this.href='https://1fichier.com/'+ '?def';">Two</a><a href="#" onclick="this.href=steal()">Bad</a>"##);
        let links:Vec<_>=html.select(&Selector::parse("a").unwrap()).map(|a|source_link(a,"https://downloadgameps3.net/game")).collect();
        assert_eq!(links[0].as_deref(),Some("https://1fichier.com/?abc")); assert_eq!(links[1].as_deref(),Some("https://1fichier.com/?def")); assert!(links[2].is_none());
        let doc=WorkItem { html:html.html(),..Default::default()}; assert_eq!(scoped_titles(&[doc],&serde_json::json!({}))[0].title_id,"PPSA12345");
    }
    #[test]
    fn archive_metadata_windows_do_not_split_unicode() {
        let html=format!("{} <a href='https://1fichier.com/?x'>Part 1</a> {}", "é🎮".repeat(130), "🎮".repeat(100));
        nearest_archive_meta(&html,"https://1fichier.com/?x");
    }

    #[test]
    fn recorded_article_and_intermediate_page_keep_seven_mirrors() {
        let r=recipe();
        let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/recorded");
        let doc=WorkItem {url:"https://dlpsgame.com/kizuna-ai-touch-the-beat-ps4-pkg/".into(),name:"Kizuna AI Touch the Beat".into(),
            html:fs::read_to_string(root.join("request-536.html")).unwrap(),..Default::default()};
        let docs=decode_fragments(vec![doc],&r["resolveSteps"][6]).unwrap();
        let titles=scoped_titles(&docs,&r["searchSteps"][7]);
        assert!(titles.iter().any(|t|t.title_id=="CUSA33096" && !t.image.is_empty()));
        let mut parts=scoped_packages(&docs,"CUSA33096",&r["resolveSteps"][7]);
        assert!(!parts.is_empty());
        for part in &mut parts {
            if part.url=="https://downloadgameps3.net/archives/41232" {
                part.html=fs::read_to_string(root.join("request-625.html")).unwrap();
            } else { panic!("unexpected intermediate {}",part.url); }
        }
        let parts=decode_fragments(parts,&r["resolveSteps"][10]).unwrap();
        let hosts=parse_hoster_links(&parts,&r["resolveSteps"][11]);
        assert_eq!(hosts.len(),7);
        assert!(hosts.iter().any(|p|p.hoster=="datanodes.to"));
        assert!(hosts.iter().all(|p|p.package_version=="1.03" && p.firmware=="5.05" && p.archive_password=="[DLPSGAME.COM]"), "metadata {:?}", hosts.iter().map(|p|(&p.package_version,&p.firmware,&p.archive_password)).collect::<Vec<_>>());
    }

    #[test]
    #[ignore = "live original-site validation; no file downloads or debrid requests"]
    fn windows_source_live_catalog_and_search() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut archive=ZipArchive::new(Cursor::new(include_bytes!("../../../../../Build-Output/Windows Manager/sources/gamesource-windows.gssource"))).unwrap();
            let mut descriptor=String::new(); archive.by_name("source.json").unwrap().read_to_string(&mut descriptor).unwrap();
            let descriptor:Descriptor=serde_json::from_str(&descriptor).unwrap(); validate_descriptor(&descriptor).unwrap();
            let mut recipe=String::new(); archive.by_name("recipe.json").unwrap().read_to_string(&mut recipe).unwrap();
            let recipe:Value=serde_json::from_str(&recipe).unwrap(); validate_recipe("recipe-v2",&recipe).unwrap();
            let catalog=recipe_search(&descriptor,&recipe,"",40).await.unwrap();
            assert!(!catalog.is_empty()); eprintln!("Live local catalog: {} titles",catalog.len());
            for query in ["Resident Evil Village", "Resident Evil"] {
                let titles=recipe_search(&descriptor,&recipe,query,30).await.unwrap(); assert!(!titles.is_empty());
                assert!(titles.iter().any(|t|t.title_id=="CUSA18008"), "PS4 Village missing: {:?}", titles.iter().map(|t|&t.title_id).collect::<Vec<_>>());
                assert!(titles.iter().any(|t|t.title_id=="PPSA01556"), "PS5 Village missing");
                let title=titles.iter().find(|t|t.title_id=="CUSA18008").unwrap();
                let packages=recipe_resolve(&descriptor,&recipe,&title.title_id,&title.name,&title.region).await.unwrap();
                assert!(!packages.is_empty());
                eprintln!("Live {query}: {} titles / {} packages",titles.len(),packages.len());
            }
        });
    }
}

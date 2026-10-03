//! Read-only DDP power observations; no wake packets or loader probes.
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Power { pub id: String, pub standby: bool }

fn parse(bytes: &[u8], version: &str) -> Option<Power> {
    let text = std::str::from_utf8(bytes).ok()?.trim_end_matches('\0');
    let mut lines = text.lines();
    let mut status = lines.next()?.split_whitespace();
    if status.next()? != "HTTP/1.1" { return None; }
    let standby = match status.next()? { "620" => true, "200" => false, _ => return None };
    let mut id = None;
    let mut matched = false;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("host-id") { id = Some(value.trim()); }
            if key.eq_ignore_ascii_case("device-discovery-protocol-version") { matched = value.trim() == version; }
        }
    }
    let id = id.filter(|id| !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_hexdigit()))?;
    matched.then(|| Power { id: id.to_ascii_lowercase(), standby })
}

pub(super) async fn observe(target: &str, host: &str) -> Option<Power> {
    let (port, version) = if target == "ps4" { (987, "00020020") } else { (9302, "00030010") };
    crate::validate_receiver_candidate(host, port).ok()?;
    tokio::time::timeout(Duration::from_secs(2), async {
        let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
        socket.connect((host, port)).await.ok()?;
        let request = format!("SRCH * HTTP/1.1\ndevice-discovery-protocol-version:{version}\n\0");
        socket.send(request.as_bytes()).await.ok()?;
        let mut bytes = [0u8; 4096];
        let count = socket.recv(&mut bytes).await.ok()?;
        parse(&bytes[..count], version)
    }).await.ok().flatten()
}

#[derive(Default)]
pub(super) struct Watch { endpoint: String, previous: Option<Power>, pending: Option<std::time::Instant> }
impl Watch {
    pub fn update(&mut self, endpoint: &str, power: Option<Power>) -> bool {
        if self.endpoint != endpoint { *self = Self { endpoint: endpoint.into(), ..Self::default() }; }
        let Some(power) = power else { return false; };
        if power.standby { self.pending = None; }
        else if self.previous.as_ref().is_some_and(|last| last.standby && last.id == power.id) { self.pending = Some(std::time::Instant::now()); }
        else if self.previous.as_ref().is_some_and(|last| last.id != power.id) { self.pending = None; }
        self.previous = Some(power);
        if self.pending.is_some_and(|at| at.elapsed() > Duration::from_secs(120)) { self.pending = None; }
        self.pending.is_some()
    }
    pub fn consume(&mut self) { self.pending = None; }
    pub fn pending(&self) -> bool { self.pending.is_some() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn power(id: &str, standby: bool) -> Option<Power> { Some(Power { id: id.into(), standby }) }
    #[test]
    fn startup_reconnect_and_other_devices_do_not_trigger_a_wake() {
        let mut watch = Watch::default();
        assert!(!watch.update("one", power("aa", false)));
        assert!(!watch.update("one", None));
        assert!(!watch.update("one", power("aa", false)));
        assert!(!watch.update("one", power("aa", true)));
        assert!(!watch.update("two", power("aa", false)));
        assert!(!watch.update("two", power("aa", true)));
        assert!(!watch.update("two", power("bb", false)));
    }
    #[test]
    fn wake_waits_for_receiver_and_is_consumed_once() {
        let mut watch = Watch::default();
        assert!(!watch.update("one", power("aa", true)));
        assert!(!watch.update("one", None));
        assert!(watch.update("one", power("aa", false)));
        assert!(watch.update("one", power("aa", false)));
        watch.consume();
        assert!(!watch.update("one", power("aa", false)));
        watch.pending = Some(std::time::Instant::now() - Duration::from_secs(121));
        assert!(!watch.update("one", power("aa", false)));
        assert!(!watch.pending());
    }
    #[test]
    fn protocol_and_identity_must_match() {
        let response = b"HTTP/1.1 620 Server Standby\r\nhost-id:aabbcc\r\ndevice-discovery-protocol-version:00030010\r\n";
        assert_eq!(parse(response,"00030010"), power("aabbcc",true));
        assert!(parse(response,"00020020").is_none());
        assert!(parse(b"HTTP/1.1 200 Ok\nhost-id:aa\n","00030010").is_none());
    }
}

//! Make the controller's sound card 4-channel so the headphone cue works.
//!
//! The DDJ family sends master on outputs 1/2 and headphones on 3/4. PipeWire usually
//! starts such a card in its stereo profile, which only exposes 1/2, so the headphones get
//! nothing. Over the PulseAudio protocol (which PipeWire speaks) Platter switches the card to
//! its "Analog Surround 4.0" profile before opening it, and puts the old profile back on exit.

use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;

use pulseaudio::protocol::{self, Command};

/// What to put back on exit.
#[derive(Clone, Debug)]
pub struct Restore {
    pub card: u32,
    pub profile: CString,
    pub card_name: String,
}

struct Conn {
    sock: BufReader<UnixStream>,
    version: u16,
    seq: u32,
}

impl Conn {
    fn open() -> Result<Conn, String> {
        let path = pulseaudio::socket_path_from_env().ok_or("no PipeWire/PulseAudio socket")?;
        let stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).ok();
        let mut sock = BufReader::new(stream);
        let cookie = pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
        let auth = protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
        protocol::write_command_message(sock.get_mut(), 0, &Command::Auth(auth), protocol::MAX_VERSION).map_err(|e| e.to_string())?;
        let (_, info) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION).map_err(|e| e.to_string())?;
        let version = protocol::MAX_VERSION.min(info.version);
        let mut props = protocol::Props::new();
        props.set(protocol::Prop::ApplicationName, CString::new("platter").unwrap());
        protocol::write_command_message(sock.get_mut(), 1, &Command::SetClientName(props), version).map_err(|e| e.to_string())?;
        protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version).map_err(|e| e.to_string())?;
        Ok(Conn { sock, version, seq: 2 })
    }

    fn cards(&mut self) -> Result<protocol::CardInfoList, String> {
        protocol::write_command_message(self.sock.get_mut(), self.seq, &Command::GetCardInfoList, self.version).map_err(|e| e.to_string())?;
        self.seq += 1;
        let (_, list) = protocol::read_reply_message::<protocol::CardInfoList>(&mut self.sock, self.version).map_err(|e| e.to_string())?;
        Ok(list)
    }

    fn set_profile(&mut self, card: u32, profile: &CString) -> Result<(), String> {
        let p = protocol::SetCardProfileParams { card_index: Some(card), card_name: None, profile_name: profile.clone() };
        protocol::write_command_message(self.sock.get_mut(), self.seq, &Command::SetCardProfile(p), self.version).map_err(|e| e.to_string())?;
        self.seq += 1;
        protocol::read_ack_message(&mut self.sock).map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn card_label(c: &protocol::CardInfo) -> String {
    let mut parts = vec![c.name.to_string_lossy().into_owned()];
    for (k, v) in c.props.iter() {
        let k = k.to_string_lossy();
        if k.starts_with("device.") || k.starts_with("alsa.") {
            parts.push(String::from_utf8_lossy(v).trim_end_matches('\0').to_string());
        }
    }
    parts.join(" ")
}

fn matches(label: &str, hints: &[String]) -> bool {
    let l = label.to_lowercase().replace([' ', '-', '_'], "");
    hints.iter().map(|h| h.to_lowercase().replace([' ', '-', '_'], "")).filter(|h| !h.is_empty()).any(|h| l.contains(&h))
}

/// The 4-channel profile to use instead of `active`, if the card has one and isn't on it.
/// Keeps the card's input (e.g. "+input:analog-stereo") when the 4.0 profile offers it.
pub fn pick_profile(active: &str, profiles: &[(String, bool)]) -> Option<String> {
    if active.contains("surround-40") || active.contains("pro-audio") {
        return None;
    }
    let input = active.split('+').find(|p| p.starts_with("input:"));
    let fours: Vec<&String> = profiles.iter().filter(|(n, avail)| *avail && n.contains("surround-40")).map(|(n, _)| n).collect();
    fours
        .iter()
        .find(|n| input.is_some_and(|i| n.contains(i)))
        .or_else(|| fours.iter().find(|n| !n.contains('+')))
        .or(fours.first())
        .map(|s| s.to_string())
}

/// Switch the controller's card to 4 channels. Ok(None) when nothing needed changing.
pub fn ensure_four_channel(hints: &[String]) -> Result<Option<Restore>, String> {
    let mut c = Conn::open()?;
    for card in c.cards()? {
        let label = card_label(&card);
        if !matches(&label, hints) {
            continue;
        }
        let active = card.active_profile.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let profiles: Vec<(String, bool)> = card.profiles.iter().map(|p| (p.name.to_string_lossy().into_owned(), p.available != 0)).collect();
        let Some(new) = pick_profile(&active, &profiles) else { return Ok(None) };
        let name = CString::new(new.clone()).map_err(|e| e.to_string())?;
        c.set_profile(card.index, &name)?;
        // Give PipeWire a moment to create the new sink before it's opened.
        std::thread::sleep(std::time::Duration::from_millis(400));
        eprintln!("audio: switched {} to {new} (headphone cue on 3/4)", card.name.to_string_lossy());
        return Ok(Some(Restore {
            card: card.index,
            profile: CString::new(active).unwrap_or_default(),
            card_name: card.name.to_string_lossy().into_owned(),
        }));
    }
    Ok(None)
}

/// The profile to restore on exit (the first one Platter changed).
static SAVED: parking_lot::Mutex<Option<Restore>> = parking_lot::const_mutex(None);

/// Switch if needed (see [`ensure_four_channel`]), remembering what to put back. Errors are
/// logged, never fatal: without PipeWire/PulseAudio there's nothing to switch.
pub fn prepare(hints: &[String]) {
    match ensure_four_channel(hints) {
        Ok(Some(r)) => {
            let mut s = SAVED.lock();
            if s.is_none() {
                *s = Some(r);
            }
        }
        Ok(None) => {}
        Err(e) => eprintln!("audio: couldn't check the controller's sound card profile: {e}"),
    }
}

/// Put back whatever [`prepare`] changed.
pub fn restore_saved() {
    if let Some(r) = SAVED.lock().take() {
        restore(&r);
    }
}

/// Put a card back the way it was.
pub fn restore(r: &Restore) {
    if r.profile.as_bytes().is_empty() {
        return;
    }
    match Conn::open().and_then(|mut c| c.set_profile(r.card, &r.profile)) {
        Ok(()) => eprintln!("audio: restored {} to {}", r.card_name, r.profile.to_string_lossy()),
        Err(e) => eprintln!("audio: couldn't restore {}'s profile: {e}", r.card_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(names: &[&str]) -> Vec<(String, bool)> {
        names.iter().map(|n| (n.to_string(), true)).collect()
    }

    #[test]
    fn picks_the_four_channel_profile() {
        let profiles = p(&["off", "output:analog-stereo", "output:analog-stereo+input:analog-stereo", "output:analog-surround-40", "output:analog-surround-40+input:analog-stereo", "pro-audio"]);
        assert_eq!(pick_profile("output:analog-stereo+input:analog-stereo", &profiles).as_deref(), Some("output:analog-surround-40+input:analog-stereo"));
        assert_eq!(pick_profile("output:analog-stereo", &profiles).as_deref(), Some("output:analog-surround-40"));
        assert_eq!(pick_profile("output:analog-surround-40", &profiles), None);
        assert_eq!(pick_profile("pro-audio", &profiles), None);
        assert_eq!(pick_profile("output:analog-stereo", &p(&["output:analog-stereo"])), None);
        let unavailable = vec![("output:analog-surround-40".to_string(), false)];
        assert_eq!(pick_profile("output:analog-stereo", &unavailable), None);
    }

    #[test]
    fn matches_controller_names() {
        let hints = vec!["GRV6".to_string(), "DDJ".to_string()];
        assert!(matches("alsa_card.usb-AlphaTheta_DDJ-GRV6-00 DDJ-GRV6", &hints));
        assert!(!matches("alsa_card.pci-0000_04_00.5 Generic HD Audio", &hints));
    }
}

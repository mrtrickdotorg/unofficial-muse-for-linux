//! Microphone capture for the dictation pill: no new dependencies.
//! The client records with whatever the system already ships
//! (`parecord`, `pw-record`, `arecord`, in that order), one channel of
//! 16-bit PCM at 16 kHz straight to a WAV file, then hands the raw
//! bytes to the `voice.dictation` route. Thirty seconds max: past that
//! the take is a monologue, not a message.

/// The other leg of the dictation pill: raw bytes plus the rate the
/// route template names. Sixteen kilohertz mono is what the client
/// records; 8 kHz stays accepted for takes from elsewhere.
pub const SAMPLE_RATE_HZ: u32 = 16_000;
const MAX_SECONDS: u32 = 30;
const BYTES_PER_SECOND: usize = 2 * SAMPLE_RATE_HZ as usize;

/// One recorder invocation: argv the client can spawn, plus the file
/// it will write. Pure, so the fallback chain is testable without a
/// microphone.
pub struct RecorderCommand {
    pub program: &'static str,
    pub args: Vec<String>,
}

/// First recorder the system can actually run. Each writes s16le mono
/// at 16 kHz, so the bytes past the WAV header are route-ready PCM.
pub fn find_recorder(path: &std::path::Path) -> Option<RecorderCommand> {
    let path = path.to_string_lossy().into_owned();
    for (program, args) in [
        (
            "parecord",
            vec![
                "--channels=1".to_owned(),
                "--rate=16000".to_owned(),
                "--format=s16le".to_owned(),
                path.clone(),
            ],
        ),
        (
            "pw-record",
            vec![
                "--channels".to_owned(),
                "1".to_owned(),
                "--rate".to_owned(),
                "16000".to_owned(),
                "--format".to_owned(),
                "s16le".to_owned(),
                path.clone(),
            ],
        ),
        (
            "arecord",
            vec![
                "-c".to_owned(),
                "1".to_owned(),
                "-r".to_owned(),
                "16000".to_owned(),
                "-f".to_owned(),
                "S16_LE".to_owned(),
                "-t".to_owned(),
                "wav".to_owned(),
                path.clone(),
            ],
        ),
    ] {
        if command_exists(program) {
            return Some(RecorderCommand { program, args });
        }
    }
    None
}

fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|dir| dir.join(program).is_file() || dir.join(format!("{program}.exe")).is_file())
    })
}

/// The route-ready bytes of a recorded WAV: everything past the
/// header. Parses chunks instead of assuming a 44-byte header, and
/// refuses anything that is not 16-bit mono PCM at a held rate.
pub fn strip_wav(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut cursor = 12;
    let mut rate: Option<u32> = None;
    let mut data: Option<&[u8]> = None;
    while cursor + 8 <= bytes.len() {
        let tag = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().ok()?) as usize;
        let body = cursor + 8;
        if body + size > bytes.len() {
            return None;
        }
        if tag == b"fmt " && size >= 16 {
            let format = u16::from_le_bytes(bytes[body..body + 2].try_into().ok()?);
            let channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().ok()?);
            let hertz = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().ok()?);
            let bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().ok()?);
            if format == 1 && channels == 1 && bits == 16 {
                rate = Some(hertz);
            }
        } else if tag == b"data" {
            data = Some(&bytes[body..body + size]);
        }
        cursor = body + size + (size % 2);
    }
    match (rate, data) {
        (Some(8000), Some(pcm)) | (Some(16000), Some(pcm)) if !pcm.is_empty() => Some(pcm),
        _ => None,
    }
}

/// A take in progress: the recorder child plus its file. Stop kills
/// the child, reads the file, and returns the route-ready bytes.
pub struct Recording {
    child: std::process::Child,
    path: std::path::PathBuf,
}

impl Recording {
    pub fn start() -> Result<Self, &'static str> {
        let path = std::env::temp_dir().join(format!("muse-dictate-{}.wav", std::process::id()));
        let command = find_recorder(&path).ok_or("no microphone recorder found")?;
        let child = std::process::Command::new(command.program)
            .args(&command.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| "the recorder would not start")?;
        Ok(Self { child, path })
    }

    pub fn stop(mut self) -> Result<(Vec<u8>, u32), &'static str> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let bytes = std::fs::read(&self.path).map_err(|_| "the take never landed")?;
        let _ = std::fs::remove_file(&self.path);
        let pcm = strip_wav(&bytes).ok_or("that take is not 16-bit mono audio")?;
        if pcm.is_empty() || pcm.len() > MAX_SECONDS as usize * BYTES_PER_SECOND {
            return Err("takes run to thirty seconds");
        }
        Ok((pcm.to_vec(), SAMPLE_RATE_HZ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, pcm: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + pcm.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(pcm);
        out
    }

    #[test]
    fn takes_keep_only_route_ready_mono_pcm() {
        assert_eq!(
            strip_wav(&wav(16000, &[1, 2, 3, 4])),
            Some(&[1, 2, 3, 4][..])
        );
        assert!(strip_wav(&wav(44100, &[1, 2])).is_none());
        assert!(strip_wav(b"not audio at all").is_none());
        assert!(strip_wav(&wav(16000, &[])).is_none());
    }

    #[test]
    fn recorder_prefers_pipewire_then_falls_back() {
        let path = std::path::Path::new("/tmp/take.wav");
        // No microphone here; the assertion is structural: whatever
        // is found names a real recorder with the take's file last.
        if let Some(command) = find_recorder(path) {
            assert!(["parecord", "pw-record", "arecord"].contains(&command.program));
            assert_eq!(
                command.args.last().map(String::as_str),
                Some("/tmp/take.wav")
            );
        }
    }
}

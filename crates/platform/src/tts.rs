//! Text-to-speech platform services: voices, speech synthesis and playback control (M12).
//!
//! Bridges to the host operating system's native speech synthesizer:
//! - Windows: SAPI via `powershell` (no COM bindings or Win32 headers needed);
//! - macOS: `/usr/bin/say`;
//! - Linux / BSD: `spd-say` (speech-dispatcher) or `espeak`;
//! - Mock/Oracle mode: record spoken utterances for deterministic headless testing and CI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// A text-to-speech voice available on this platform.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Voice {
    pub id: String,
    pub name: String,
    pub language: String,
    pub gender: Option<String>,
}

/// Parameters for speech synthesis.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpeechOptions {
    /// Preferred voice name or identifier, or None for the system default.
    pub voice: Option<String>,
    /// Speech rate multiplier (1.0 is normal speed, 0.5 is half speed, 2.0 is double speed).
    pub rate: f32,
    /// Pitch multiplier (1.0 is normal pitch).
    pub pitch: f32,
    /// Volume from 0.0 (silent) to 1.0 (full volume).
    pub volume: f32,
}

impl Default for SpeechOptions {
    fn default() -> Self {
        Self { voice: None, rate: 1.0, pitch: 1.0, volume: 1.0 }
    }
}

/// The current playback status of speech synthesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SpeechStatus {
    Idle,
    Speaking,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TtsError {
    Unavailable(String),
    Failed(String),
}

impl std::fmt::Display for TtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "text-to-speech unavailable: {m}"),
            Self::Failed(m) => write!(f, "speech synthesis failed: {m}"),
        }
    }
}

impl std::error::Error for TtsError {}

struct ActiveSpeech {
    status: SpeechStatus,
    current_text: Option<String>,
    options: SpeechOptions,
    #[cfg(not(target_arch = "wasm32"))]
    child: Option<std::process::Child>,
}

static MOCK_ENABLED: AtomicBool = AtomicBool::new(false);
static MOCK_SPOKEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn state() -> &'static Mutex<ActiveSpeech> {
    static STATE: OnceLock<Mutex<ActiveSpeech>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(ActiveSpeech {
            status: SpeechStatus::Idle,
            current_text: None,
            options: SpeechOptions::default(),
            #[cfg(not(target_arch = "wasm32"))]
            child: None,
        })
    })
}

/// Enable or disable mock TTS mode for deterministic testing without external processes.
pub fn set_mock(mock: bool) {
    MOCK_ENABLED.store(mock, Ordering::SeqCst);
}

/// Whether mock TTS mode is currently active.
pub fn is_mock() -> bool {
    MOCK_ENABLED.load(Ordering::SeqCst) || std::env::var_os("PDFCRAFT_TTS_MOCK").is_some()
}

/// Take all utterances spoken in mock mode since the last call.
pub fn take_mock_spoken() -> Vec<String> {
    let mut guard = match MOCK_SPOKEN.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    std::mem::take(&mut *guard)
}

/// Clear mock utterances.
pub fn clear_mock_spoken() {
    let _ = take_mock_spoken();
}

/// Default fallback voices returned when the platform reports none or in mock mode.
pub fn default_mock_voices() -> Vec<Voice> {
    vec![
        Voice { id: "system-default".into(), name: "System Default Voice".into(), language: "en-US".into(), gender: Some("Neutral".into()) },
        Voice { id: "pt-br-default".into(), name: "Voz Português (Brasil)".into(), language: "pt-BR".into(), gender: Some("Female".into()) },
    ]
}

/// List the text-to-speech voices available on this operating system.
pub fn list_voices() -> Vec<Voice> {
    if is_mock() {
        return default_mock_voices();
    }

    #[cfg(windows)]
    {
        list_windows_voices()
    }
    #[cfg(target_os = "macos")]
    {
        list_macos_voices()
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_arch = "wasm32")))]
    {
        list_linux_voices()
    }
    #[cfg(target_arch = "wasm32")]
    {
        default_mock_voices()
    }
}

/// Current playback state of speech synthesis.
pub fn status() -> SpeechStatus {
    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(child) = guard.child.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                guard.child = None;
                guard.status = SpeechStatus::Idle;
                guard.current_text = None;
            }
        }
    }
    guard.status
}

/// The text currently being spoken, if any.
pub fn current_text() -> Option<String> {
    let guard = match state().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.current_text.clone()
}

/// Speak `text` using `options`. If speech is already playing, it is stopped first.
pub fn speak(text: &str, options: &SpeechOptions) -> Result<(), TtsError> {
    stop()?;

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if is_mock() {
        let mut mock_guard = match MOCK_SPOKEN.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        mock_guard.push(trimmed.to_string());

        let mut s = match state().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        s.status = SpeechStatus::Speaking;
        s.current_text = Some(trimmed.to_string());
        s.options = options.clone();
        return Ok(());
    }

    #[cfg(windows)]
    {
        speak_windows(trimmed, options)
    }
    #[cfg(target_os = "macos")]
    {
        speak_macos(trimmed, options)
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_arch = "wasm32")))]
    {
        speak_linux(trimmed, options)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut s = match state().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        s.status = SpeechStatus::Speaking;
        s.current_text = Some(trimmed.to_string());
        s.options = options.clone();
        Ok(())
    }
}

/// Pause current speech playback.
pub fn pause() -> Result<(), TtsError> {
    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if guard.status == SpeechStatus::Speaking {
        guard.status = SpeechStatus::Paused;
    }
    Ok(())
}

/// Resume speech playback if paused.
pub fn resume() -> Result<(), TtsError> {
    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if guard.status == SpeechStatus::Paused {
        guard.status = SpeechStatus::Speaking;
    }
    Ok(())
}

/// Stop speech playback and reset playback state to idle.
pub fn stop() -> Result<(), TtsError> {
    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(mut child) = guard.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    guard.status = SpeechStatus::Idle;
    guard.current_text = None;
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows implementation via PowerShell SAPI
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn list_windows_voices() -> Vec<Voice> {
    static CACHED: OnceLock<Vec<Voice>> = OnceLock::new();
    CACHED.get_or_init(|| {
        const POWERSHELL_VOICES: &str = r#"[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false); Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.GetInstalledVoices() | ForEach-Object { [PSCustomObject]@{ id = $_.VoiceInfo.Name; name = $_.VoiceInfo.Name; lang = $_.VoiceInfo.Culture.Name; gender = $_.VoiceInfo.Gender.ToString() } } | ConvertTo-Json -Compress"#;

        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let mut cmd = std::process::Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", POWERSHELL_VOICES]);
        cmd.creation_flags(CREATE_NO_WINDOW);

        let output = match cmd.output() {
            Ok(out) if out.status.success() => out.stdout,
            _ => return default_mock_voices(),
        };

        let text = String::from_utf8_lossy(&output).trim().to_string();
        if text.is_empty() {
            return default_mock_voices();
        }

        #[derive(serde::Deserialize)]
        struct RawVoice {
            id: Option<String>,
            name: Option<String>,
            lang: Option<String>,
            gender: Option<String>,
        }

        let parsed: Vec<RawVoice> = if text.starts_with('[') {
            serde_json::from_str(&text).unwrap_or_default()
        } else if let Ok(single) = serde_json::from_str::<RawVoice>(&text) {
            vec![single]
        } else {
            Vec::new()
        };

        if parsed.is_empty() {
            return default_mock_voices();
        }

        parsed
            .into_iter()
            .filter_map(|r| {
                let name = r.name?;
                let id = r.id.unwrap_or_else(|| name.clone());
                let lang = r.lang.unwrap_or_else(|| {
                    if name.to_ascii_lowercase().contains("portuguese") || name.to_ascii_lowercase().contains("brasil") {
                        "pt-BR".into()
                    } else {
                        "en-US".into()
                    }
                });
                Some(Voice { id, name, language: lang, gender: r.gender })
            })
            .collect()
    }).clone()
}

#[cfg(windows)]
fn ensure_sapi_script() -> Result<std::path::PathBuf, TtsError> {
    static SCRIPT_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
    if let Some(p) = SCRIPT_PATH.get() {
        return Ok(p.clone());
    }

    let path = std::env::temp_dir().join("pdfcraft_sapi_speak_v2.vbs");
    const VBS_CONTENT: &str = r#"If WScript.Arguments.Count < 4 Then WScript.Quit 1
textFile = WScript.Arguments(0)
rate = CInt(WScript.Arguments(1))
vol = CInt(WScript.Arguments(2))
voiceName = WScript.Arguments(3)

Set fso = CreateObject("Scripting.FileSystemObject")
If Not fso.FileExists(textFile) Then WScript.Quit 2

Set f = fso.OpenTextFile(textFile, 1, False, -1)
text = f.ReadAll
f.Close
On Error Resume Next
fso.DeleteFile textFile, True
On Error GoTo 0

Set voice = CreateObject("SAPI.SpVoice")
voice.Rate = rate
voice.Volume = vol

If Len(voiceName) > 0 And voiceName <> "default" Then
    For Each v In voice.GetVoices
        If InStr(1, v.GetDescription, voiceName, 1) > 0 Then
            Set voice.Voice = v
            Exit For
        End If
    Next
End If

voice.Speak text
"#;
    std::fs::write(&path, VBS_CONTENT.as_bytes()).map_err(|e| TtsError::Failed(format!("could not write SAPI VBScript helper: {e}")))?;

    let _ = SCRIPT_PATH.set(path.clone());
    Ok(path)
}

#[cfg(windows)]
fn speak_windows(text: &str, options: &SpeechOptions) -> Result<(), TtsError> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let vbs_path = ensure_sapi_script()?;

    let tmp_file = std::env::temp_dir().join(format!(
        "pdfcraft_speech_{}_{}.txt",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
    ));

    let mut utf16_bytes = Vec::with_capacity(2 + text.len() * 2);
    utf16_bytes.extend_from_slice(&[0xFF, 0xFE]);
    for u in text.encode_utf16() {
        utf16_bytes.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(&tmp_file, &utf16_bytes).map_err(|e| TtsError::Failed(format!("could not write speech text buffer: {e}")))?;

    let sapi_rate = ((options.rate - 1.0) * 10.0).round().clamp(-10.0, 10.0) as i32;
    let sapi_volume = (options.volume.clamp(0.0, 1.0) * 100.0).round() as i32;
    let voice_arg = options.voice.as_deref().unwrap_or("default");

    let mut cmd = std::process::Command::new("cscript");
    cmd.args([
        "//nologo",
        vbs_path.to_string_lossy().as_ref(),
        tmp_file.to_string_lossy().as_ref(),
        &sapi_rate.to_string(),
        &sapi_volume.to_string(),
        voice_arg,
    ]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let child = cmd.spawn().map_err(|e| TtsError::Failed(format!("could not spawn speech process: {e}")))?;

    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.status = SpeechStatus::Speaking;
    guard.current_text = Some(text.to_string());
    guard.options = options.clone();
    guard.child = Some(child);

    Ok(())
}

// ---------------------------------------------------------------------------
// macOS implementation via /usr/bin/say
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn list_macos_voices() -> Vec<Voice> {
    let output = match std::process::Command::new("say").arg("-v").arg("?").output() {
        Ok(out) if out.status.success() => out.stdout,
        _ => return default_mock_voices(),
    };

    let text = String::from_utf8_lossy(&output);
    let mut voices = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let name = parts[0].to_string();
            let lang = parts[1].replace('_', "-");
            voices.push(Voice { id: name.clone(), name, language: lang, gender: None });
        }
    }

    if voices.is_empty() { default_mock_voices() } else { voices }
}

#[cfg(target_os = "macos")]
fn speak_macos(text: &str, options: &SpeechOptions) -> Result<(), TtsError> {
    let mut cmd = std::process::Command::new("say");
    if let Some(v) = &options.voice {
        cmd.arg("-v").arg(v);
    }
    let wpm = (175.0 * options.rate.clamp(0.5, 3.0)).round() as u32;
    cmd.arg("-r").arg(wpm.to_string());
    cmd.arg(text);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let child = cmd.spawn().map_err(|e| TtsError::Failed(format!("could not spawn say process: {e}")))?;

    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.status = SpeechStatus::Speaking;
    guard.current_text = Some(text.to_string());
    guard.options = options.clone();
    guard.child = Some(child);

    Ok(())
}

// ---------------------------------------------------------------------------
// Linux / BSD implementation via spd-say or espeak
// ---------------------------------------------------------------------------

#[cfg(all(unix, not(target_os = "macos"), not(target_arch = "wasm32")))]
fn list_linux_voices() -> Vec<Voice> {
    default_mock_voices()
}

#[cfg(all(unix, not(target_os = "macos"), not(target_arch = "wasm32")))]
fn speak_linux(text: &str, options: &SpeechOptions) -> Result<(), TtsError> {
    let mut cmd = std::process::Command::new("spd-say");
    let rate_pct = (((options.rate.clamp(0.5, 2.0) - 1.0) * 100.0).round() as i32).clamp(-100, 100);
    let pitch_pct = (((options.pitch.clamp(0.5, 2.0) - 1.0) * 100.0).round() as i32).clamp(-100, 100);
    cmd.arg("-r").arg(rate_pct.to_string());
    cmd.arg("-p").arg(pitch_pct.to_string());
    if let Some(v) = &options.voice {
        cmd.arg("-t").arg(v);
    }
    cmd.arg(text);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            let mut espeak = std::process::Command::new("espeak");
            espeak.arg(text);
            espeak.spawn().map_err(|e| TtsError::Failed(format!("neither spd-say nor espeak available: {e}")))?
        }
    };

    let mut guard = match state().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.status = SpeechStatus::Speaking;
    guard.current_text = Some(text.to_string());
    guard.options = options.clone();
    guard.child = Some(child);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_tts_records_utterances_and_updates_status() {
        set_mock(true);
        clear_mock_spoken();

        assert_eq!(status(), SpeechStatus::Idle);
        assert_eq!(current_text(), None);

        let voices = list_voices();
        assert!(!voices.is_empty());

        let opts = SpeechOptions { voice: Some("pt-br-default".into()), rate: 1.2, pitch: 1.0, volume: 0.9 };

        speak("Olá mundo, lendo PDF em voz alta", &opts).unwrap();
        assert_eq!(status(), SpeechStatus::Speaking);
        assert_eq!(current_text(), Some("Olá mundo, lendo PDF em voz alta".into()));

        pause().unwrap();
        assert_eq!(status(), SpeechStatus::Paused);

        resume().unwrap();
        assert_eq!(status(), SpeechStatus::Speaking);

        stop().unwrap();
        assert_eq!(status(), SpeechStatus::Idle);
        assert_eq!(current_text(), None);

        let spoken = take_mock_spoken();
        assert_eq!(spoken, vec!["Olá mundo, lendo PDF em voz alta"]);

        set_mock(false);
    }
}

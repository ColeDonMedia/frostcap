//! Rebuild a project whose recording session crashed (status `recording` and no
//! live session). Uses the sidecars written while recording:
//! `session.json` (t0, pauses, heartbeat) and `input.jsonl` (raw input events).
//! WAV headers are re-patched to the real data length.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::clock::HNS_PER_MS;
use crate::error::{Error, Result};
use crate::input::read_sidecar;
use crate::project::{
    AudioTrack, MIC_AUDIO_FILE, Project, ProjectStatus, SCREEN_FILE, SYSTEM_AUDIO_FILE,
    project_dir_of,
};
/// Max accepted size of `session.json`.
const MAX_SESSION_SIDECAR: u64 = 1 << 20;

use crate::project::{VideoTrack, WEBCAM_FILE};
use crate::recorder::{INPUT_SIDECAR, SESSION_SIDECAR, SessionSidecar, clamp_tracks, pause_gaps};
use crate::timeline::Gap;

/// Re-patch a WAV written by [`crate::audio`] to its full length. Returns
/// (sample_rate, channels, frames).
fn repair_wav(path: &Path) -> Option<(u32, u16, u64)> {
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .ok()?;
    let len = f.metadata().ok()?.len();
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12u64;
    let (mut rate, mut ch, mut block) = (0u32, 0u16, 0u16);
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut ck = [0u8; 8];
        f.read_exact(&mut ck).ok()?;
        let size = u64::from(u32::from_le_bytes(ck[4..8].try_into().ok()?));
        if &ck[0..4] == b"fmt " {
            let mut fmt = [0u8; 16];
            f.read_exact(&mut fmt).ok()?;
            ch = u16::from_le_bytes([fmt[2], fmt[3]]);
            rate = u32::from_le_bytes(fmt[4..8].try_into().ok()?);
            block = u16::from_le_bytes([fmt[12], fmt[13]]);
        } else if &ck[0..4] == b"data" {
            let data = len - (pos + 8);
            let data = data - data % u64::from(block.max(1));
            f.seek(SeekFrom::Start(4)).ok()?;
            f.write_all(&u32::try_from(len - 8).unwrap_or(u32::MAX).to_le_bytes())
                .ok()?;
            f.seek(SeekFrom::Start(pos + 4)).ok()?;
            f.write_all(&u32::try_from(data).unwrap_or(u32::MAX).to_le_bytes())
                .ok()?;
            f.sync_all().ok()?;
            return (rate > 0).then(|| (rate, ch, data / u64::from(block.max(1))));
        }
        pos += 8 + size + size % 2;
    }
    None
}

/// Privacy scrub of a finished project: drop keys and/or window titles, save,
/// and delete any leftover recovery sidecars. Interrupted recordings must be
/// recovered first.
pub fn scrub_project_dir(dir: &Path, keys: bool, titles: bool) -> Result<Project> {
    let mut p = Project::load(dir)?;
    if p.status == ProjectStatus::Recording {
        return Err(Error::Invalid(
            "recover this interrupted recording before scrubbing it".into(),
        ));
    }
    p.scrub(keys, titles);
    p.save(dir)?;
    for f in [INPUT_SIDECAR, SESSION_SIDECAR] {
        let path = dir.join(f);
        if path.exists() {
            fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
        }
    }
    Ok(p)
}

/// Recover a crashed project in place and return it (`status: "recovered"`).
pub fn recover_project(path: &Path) -> Result<Project> {
    let dir = project_dir_of(path);
    let mut project = Project::load(&dir)?;
    if project.status != ProjectStatus::Recording {
        return Err(Error::Invalid(
            "project is not an interrupted recording".into(),
        ));
    }
    let session: SessionSidecar = fs::metadata(dir.join(SESSION_SIDECAR))
        .ok()
        .filter(|m| m.len() <= MAX_SESSION_SIDECAR)
        .and_then(|_| fs::read(dir.join(SESSION_SIDECAR)).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut warnings = vec!["recovered after an interrupted recording".to_owned()];
    let gaps: Vec<Gap> = session
        .gaps
        .iter()
        .map(|&(start, end)| Gap { start, end })
        .collect();
    let heartbeat_ms = match session.t0_hns {
        Some(t0) => {
            let end = session.paused_since.unwrap_or(session.heartbeat_hns);
            let removed = gaps.iter().fold(0i64, |acc, g| {
                acc.saturating_add(g.end.saturating_sub(g.start).max(0))
            });
            (end.saturating_sub(t0).saturating_sub(removed).max(0) / HNS_PER_MS) as u64
        }
        None => 0,
    };
    // A hostile or corrupt sidecar/mp4 must not produce an unloadable project.
    let heartbeat_ms = heartbeat_ms.min(crate::project::limits::MAX_DURATION_MS);
    // The fragmented screen.mp4 is the ground truth for how much video survived.
    let screen = crate::mp4::inspect(&dir.join(SCREEN_FILE));
    let duration_ms = match &screen {
        Some(i) if i.playable() => i.duration_ms,
        _ => {
            warnings.push("screen.mp4 was not finalized and may not play".into());
            heartbeat_ms
        }
    };
    let duration_ms = duration_ms.min(crate::project::limits::MAX_DURATION_MS);
    if let Some(w) =
        crate::mp4::inspect(&dir.join(WEBCAM_FILE)).filter(crate::mp4::Mp4Info::playable)
    {
        project.media.webcam = Some(WEBCAM_FILE.into());
        project.media.webcam_video = Some(VideoTrack {
            file: WEBCAM_FILE.into(),
            name: "Camera".into(),
            width: w.width,
            height: w.height,
            offset_ms: 0,
            duration_ms: w.duration_ms,
        });
    }
    if let Some(t0) = session.t0_hns {
        let mut tracks = read_sidecar(&dir.join(INPUT_SIDECAR)).into_tracks(t0, &gaps);
        clamp_tracks(&mut tracks, duration_ms);
        project.tracks = tracks;
        project.pauses = pause_gaps(t0, &gaps);
    } else {
        warnings.push("no video frame was recorded before the interruption".into());
    }

    for (file, slot) in [
        (MIC_AUDIO_FILE, &mut project.media.mic_audio),
        (SYSTEM_AUDIO_FILE, &mut project.media.system_audio),
    ] {
        if let Some((sample_rate, channels, frames)) = repair_wav(&dir.join(file)) {
            *slot = Some(AudioTrack {
                file: file.into(),
                offset_ms: 0,
                sample_rate,
                channels,
                duration_ms: frames * 1000 / u64::from(sample_rate.max(1)),
            });
        }
    }
    project.duration_ms = duration_ms;
    project.status = ProjectStatus::Recovered;
    project.warnings.extend(warnings);
    project.save(&dir)?;
    let _ = fs::remove_file(dir.join(INPUT_SIDECAR));
    let _ = fs::remove_file(dir.join(SESSION_SIDECAR));
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::SidecarEvent;
    use crate::project::{MediaFiles, PROJECT_VERSION, Rect, Size, SourceDescriptor, SourceKind};

    #[test]
    fn scrub_removes_keys_titles_and_sidecars() {
        let dir = crate::test_tmp_dir().join(format!("scrub-{}.frostcap", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut p = crate::project::samples::fixture();
        p.tracks.focus[0].title = "Bank".into();
        p.save(&dir).unwrap();
        fs::write(dir.join(INPUT_SIDECAR), b"{}").unwrap();
        fs::write(dir.join(SESSION_SIDECAR), b"{}").unwrap();
        let s = scrub_project_dir(&dir, true, true).unwrap();
        assert!(s.tracks.keys.is_empty() && s.tracks.focus.iter().all(|f| f.title.is_empty()));
        assert!(!dir.join(INPUT_SIDECAR).exists() && !dir.join(SESSION_SIDECAR).exists());
        assert_eq!(Project::load(&dir).unwrap(), s);
        // An interrupted recording must be recovered first (sidecars are its data).
        let mut r = s.clone();
        r.status = ProjectStatus::Recording;
        r.save(&dir).unwrap();
        assert!(scrub_project_dir(&dir, true, true).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn absurd_heartbeat_durations_are_clamped_and_still_save() {
        let dir =
            crate::test_tmp_dir().join(format!("recover-huge-{}.frostcap", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut p = crate::project::samples::sample();
        p.status = ProjectStatus::Recording;
        p.save(&dir).unwrap();
        let s = SessionSidecar {
            t0_hns: Some(0),
            gaps: vec![],
            paused_since: None,
            heartbeat_hns: i64::MAX,
        };
        fs::write(dir.join(SESSION_SIDECAR), serde_json::to_vec(&s).unwrap()).unwrap();
        let r = recover_project(&dir).unwrap();
        assert_eq!(r.duration_ms, crate::project::limits::MAX_DURATION_MS);
        Project::load(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn attaches_fragmented_screen_and_webcam() {
        let dir =
            crate::test_tmp_dir().join(format!("recover-fmp4-{}.frostcap", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut p = crate::project::samples::sample();
        p.status = ProjectStatus::Recording;
        p.duration_ms = 0;
        p.save(&dir).unwrap();
        let ms = HNS_PER_MS;
        // Heartbeat says ~9 s, but only 2.5 s of video were flushed before the crash.
        let s = SessionSidecar {
            t0_hns: Some(1_000 * ms),
            gaps: vec![(3_000 * ms, 4_000 * ms)],
            paused_since: Some(9_000 * ms),
            heartbeat_hns: 10_000 * ms,
        };
        fs::write(dir.join(SESSION_SIDECAR), serde_json::to_vec(&s).unwrap()).unwrap();
        fs::write(
            dir.join(SCREEN_FILE),
            crate::mp4::test_util::fragmented(&[&[10_000, 10_000], &[5_000]]),
        )
        .unwrap();
        fs::write(
            dir.join(crate::project::WEBCAM_FILE),
            crate::mp4::test_util::fragmented(&[&[10_000, 10_000]]),
        )
        .unwrap();
        let r = recover_project(&dir).unwrap();
        assert_eq!(
            r.duration_ms, 2_500,
            "video duration wins over the heartbeat"
        );
        let w = r.media.webcam_video.as_ref().expect("webcam attached");
        assert_eq!(w.duration_ms, 2_000);
        assert_eq!(r.media.webcam.as_deref(), Some(crate::project::WEBCAM_FILE));
        assert!(
            !r.warnings.iter().any(|w| w.contains("not finalized")),
            "{:?}",
            r.warnings
        );
        assert_eq!(r.pauses.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovers_tracks_audio_and_flags_unfinalized_video() {
        let dir = crate::test_tmp_dir().join(format!("recover-{}.frostcap", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = Project {
            version: PROJECT_VERSION,
            name: "crash".into(),
            created_at_ms: 1,
            source: SourceDescriptor {
                kind: SourceKind::Monitor,
                name: "m".into(),
                bounds: Rect::default(),
                crop: None,
            },
            canvas: Size {
                width: 1920,
                height: 1080,
            },
            fps: 60,
            duration_ms: 0,
            media: MediaFiles {
                screen: SCREEN_FILE.into(),
                ..Default::default()
            },
            tracks: Default::default(),
            edits: Default::default(),
            pauses: vec![],
            status: ProjectStatus::Recording,
            warnings: vec![],
            cursor_baked: false,
        };
        p.save(&dir).unwrap();
        let ms = HNS_PER_MS;
        let s = SessionSidecar {
            t0_hns: Some(1_000 * ms),
            gaps: vec![],
            paused_since: None,
            heartbeat_hns: 4_000 * ms,
        };
        fs::write(dir.join(SESSION_SIDECAR), serde_json::to_vec(&s).unwrap()).unwrap();
        let lines = [
            SidecarEvent::C {
                ts: 1_500 * ms,
                x: 10,
                y: 20,
            },
            SidecarEvent::C {
                ts: 9_000 * ms,
                x: 11,
                y: 21,
            }, // after heartbeat -> clamped away
        ];
        let mut text: String = lines
            .iter()
            .map(|l| serde_json::to_string(l).unwrap() + "\n")
            .collect();
        text.push_str("{\"k\":\"c\",\"ts\":"); // torn line
        fs::write(dir.join(INPUT_SIDECAR), text).unwrap();
        fs::write(dir.join(SCREEN_FILE), b"\0\0\0\x10ftypisom\0\0\0\0").unwrap();
        // WAV with a stale (zero) data size, 48 kHz stereo f32 -> block 8.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&3u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&(48_000u32 * 8).to_le_bytes());
        wav.extend_from_slice(&8u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data\0\0\0\0");
        wav.extend(std::iter::repeat_n(0u8, 48_000 * 8));
        fs::write(dir.join(MIC_AUDIO_FILE), wav).unwrap();

        let r = recover_project(&dir).unwrap();
        assert_eq!(r.status, ProjectStatus::Recovered);
        assert_eq!(r.duration_ms, 3_000);
        assert_eq!(r.tracks.cursor.len(), 1);
        assert_eq!(r.tracks.cursor[0].t_ms, 500);
        assert_eq!(r.media.mic_audio.as_ref().unwrap().duration_ms, 1_000);
        assert!(r.warnings.iter().any(|w| w.contains("not finalized")));
        assert!(!dir.join(INPUT_SIDECAR).exists());
        assert!(
            recover_project(&dir).is_err(),
            "second recovery must be refused"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

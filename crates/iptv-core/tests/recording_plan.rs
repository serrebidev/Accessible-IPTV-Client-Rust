use std::collections::BTreeMap;
use std::path::Path;

use iptv_core::recording::{plan_recording, sanitize_filename, RecordingOptions, FORMATS};
use iptv_core::Channel;

fn channel(name: &str, url: &str) -> Channel {
    Channel {
        name: name.into(),
        url: url.into(),
        group: String::new(),
        tvg_id: String::new(),
        tvg_name: String::new(),
        tvg_logo: String::new(),
        unknown_attributes: BTreeMap::new(),
    }
}

fn plan(channel: &Channel, format: &str) -> iptv_core::recording::RecordingPlan {
    plan_recording(
        channel,
        Path::new("recordings"),
        "2026-11-01 12-34-56",
        RecordingOptions {
            format,
            ..RecordingOptions::default()
        },
        &[],
        |_| false,
    )
    .unwrap()
}

#[test]
fn every_format_has_python_metadata_and_muxer_on_part_output() {
    let expected = [
        ("provider_mkv", "mkv", "video", "matroska", false),
        ("provider_mp4", "mp4", "video", "mp4", true),
        ("x264_mkv", "mkv", "video", "matroska", false),
        ("x264_mp4", "mp4", "video", "mp4", true),
        ("provider_mka", "mka", "audio", "matroska", false),
        ("audio_mp3_v0", "mp3", "audio", "mp3", false),
        ("audio_flac", "flac", "audio", "flac", false),
        ("audio_wav", "wav", "audio", "wav", false),
        ("audio_aac_m4a", "m4a", "audio", "ipod", false),
        ("audio_opus", "opus", "audio", "oga", false),
    ];
    assert_eq!(FORMATS.len(), expected.len());
    for (key, ext, kind, muxer, faststart) in expected {
        let result = plan(&channel("News", "http://example.invalid/live"), key);
        assert_eq!(result.format.key, key);
        assert_eq!(result.format.extension, ext);
        assert_eq!(result.format.kind, kind);
        assert_eq!(result.format.muxer, muxer);
        assert_eq!(result.format.faststart, faststart);
        assert!(!result.format.ffmpeg_options.is_empty());
        assert!(result
            .output_path
            .to_string_lossy()
            .ends_with(&format!(".{ext}")));
        assert!(result
            .part_path
            .to_string_lossy()
            .ends_with(&format!(".{ext}.part")));
        assert_eq!(
            &result.argv[result.argv.len() - 3..],
            ["-f", muxer, result.part_path.to_str().unwrap()]
        );
        assert_eq!(result.argv.contains(&"+faststart".to_string()), faststart);
    }
}

#[test]
fn exact_http_argv_reuses_channel_headers_and_hides_secrets_in_display() {
    let mut input = channel("News", "https://user:password@example.invalid/live?token=secret|Referer=https://ref.invalid/|Cookie=from=url");
    input
        .unknown_attributes
        .insert("http-user-agent".into(), "PrivateAgent".into());
    input
        .unknown_attributes
        .insert("http-cookie".into(), "session=private".into());
    let result = plan(&input, "provider_mp4");
    let path = result.part_path.to_str().unwrap();
    assert_eq!(
        result.argv,
        vec![
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "level+info",
            "-n",
            "-nostats",
            "-rw_timeout",
            "15000000",
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_delay_max",
            "5",
            "-user_agent",
            "PrivateAgent",
            "-referer",
            "https://ref.invalid/",
            "-headers",
            "Cookie: session=private\r\n",
            "-i",
            "https://user:password@example.invalid/live?token=secret",
            "-map",
            "0:v?",
            "-map",
            "0:a?",
            "-dn",
            "-sn",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
            path,
        ]
    );
    let display = format!("{result:?} {}", result.display_label);
    for secret in [
        "password",
        "token=secret",
        "PrivateAgent",
        "session=private",
        "https://user:",
    ] {
        assert!(!display.contains(secret), "display leaked {secret}");
    }
}

#[test]
fn format_families_map_and_encode_like_python() {
    let input = channel("Radio", "rtsp://example.invalid/live");
    let cases = [
        ("provider_mkv", vec!["-map", "0", "-c", "copy"]),
        (
            "provider_mka",
            vec!["-map", "0:a?", "-vn", "-sn", "-dn", "-c:a", "copy"],
        ),
        (
            "x264_mkv",
            vec![
                "-map", "0:v?", "-map", "0:a?", "-c:v", "libx264", "-preset", "veryfast", "-crf",
                "21", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k",
            ],
        ),
        (
            "audio_mp3_v0",
            vec!["-vn", "-c:a", "libmp3lame", "-q:a", "0"],
        ),
        ("audio_flac", vec!["-vn", "-c:a", "flac"]),
        ("audio_wav", vec!["-vn", "-c:a", "pcm_s16le"]),
        ("audio_aac_m4a", vec!["-vn", "-c:a", "aac", "-b:a", "256k"]),
        ("audio_opus", vec!["-vn", "-c:a", "libopus", "-b:a", "160k"]),
    ];
    for (key, sequence) in cases {
        let result = plan(&input, key);
        assert!(result.argv.windows(sequence.len()).any(|w| w == sequence));
        assert!(!result.argv.contains(&"-reconnect".to_string()));
        assert!(result
            .argv
            .windows(2)
            .any(|w| w == ["-rtsp_transport", "tcp"]));
    }
    assert_eq!(plan(&input, "unknown").format.key, "provider_mkv");
}

#[test]
fn chosen_audio_track_sets_dispositions_and_audio_only_map() {
    let input = channel("News", "http://example.invalid/live");
    let options = RecordingOptions {
        format: "x264_mp4",
        audio_track: Some(1),
        audio_track_count: 3,
        ..RecordingOptions::default()
    };
    let result = plan_recording(
        &input,
        Path::new("recordings"),
        "2026-11-01 12-34-56",
        options,
        &[],
        |_| false,
    )
    .unwrap();
    assert!(result.argv.windows(6).any(|w| w
        == [
            "-disposition:a:0",
            "-default",
            "-disposition:a:1",
            "+default",
            "-disposition:a:2",
            "-default"
        ]));
    let options = RecordingOptions {
        format: "audio_flac",
        audio_track: Some(1),
        ..RecordingOptions::default()
    };
    let result = plan_recording(
        &input,
        Path::new("recordings"),
        "2026-11-01 12-34-56",
        options,
        &[],
        |_| false,
    )
    .unwrap();
    assert!(result.argv.windows(2).any(|w| w == ["-map", "0:a:1"]));
    assert!(!result
        .argv
        .iter()
        .any(|arg| arg.starts_with("-disposition")));
}

#[test]
fn sanitization_and_collisions_protect_completed_and_part_paths() {
    assert_eq!(sanitize_filename(" A/B:C*?\"<>| D. "), "A B C D");
    assert_eq!(sanitize_filename("..."), "Recording");
    assert_eq!(sanitize_filename("CON"), "CON_");
    assert_eq!(sanitize_filename("LPT1.txt"), "LPT1.txt_");
    assert_eq!(sanitize_filename("Émission 東京"), "Émission 東京");
    assert_eq!(sanitize_filename(&"é".repeat(150)).chars().count(), 120);
    let input = channel("Show", "http://example.invalid/live");
    let first = plan(&input, "provider_mkv");
    let second = plan_recording(
        &input,
        Path::new("recordings"),
        "2026-11-01 12-34-56",
        RecordingOptions::default(),
        &[],
        |p| p == first.part_path,
    )
    .unwrap();
    assert!(second
        .output_path
        .to_string_lossy()
        .ends_with("Show - 2026-11-01 12-34-56 (2).mkv"));
    let third = plan_recording(
        &input,
        Path::new("recordings"),
        "2026-11-01 12-34-56",
        RecordingOptions::default(),
        &[first.output_path, second.output_path],
        |_| false,
    )
    .unwrap();
    assert!(third
        .output_path
        .to_string_lossy()
        .ends_with("Show - 2026-11-01 12-34-56 (3).mkv"));
}

#[test]
fn url_shaped_channel_name_is_not_a_display_diagnostic() {
    let input = channel(
        "https://user:pass@example.invalid/live",
        "https://user:pass@example.invalid/live",
    );
    let result = plan(&input, "provider_mkv");
    assert_eq!(result.display_label, "Recording");
    assert!(!format!("{result:?}").contains("pass"));
}

#[test]
fn ordinary_at_sign_in_channel_name_is_preserved() {
    let result = plan(
        &channel("K@Home TV", "https://example.invalid/live"),
        "provider_mkv",
    );
    assert_eq!(result.display_label, "K@Home TV");
}

#[test]
fn channel_extra_headers_and_authorization_reach_http_input_only() {
    let mut input = channel(
        "Sports",
        "http://example.invalid/live|Origin=https://origin.invalid",
    );
    input
        .unknown_attributes
        .insert("http-authorization".into(), "Bearer private".into());
    input
        .unknown_attributes
        .insert("http-accept".into(), "video/*".into());
    input
        .unknown_attributes
        .insert("http-headers".into(), "X-Token: secret".into());
    let result = plan(&input, "provider_mkv");
    let index = result
        .argv
        .iter()
        .position(|arg| arg == "-headers")
        .unwrap();
    assert_eq!(result.argv[index + 1], "Authorization: Bearer private\r\nAccept: video/*\r\nOrigin: https://origin.invalid\r\nX-Token: secret\r\n");
    assert!(index < result.argv.iter().position(|arg| arg == "-i").unwrap());
    let display = format!("{result:?}");
    assert!(!display.contains("private"));
    assert!(!display.contains("secret"));

    input.url = "rtmp://example.invalid/live".into();
    let result = plan(&input, "provider_mkv");
    assert!(!result.argv.contains(&"-headers".into()));
    assert!(!result.argv.contains(&"-user_agent".into()));
}

#[test]
fn encoded_url_modifiers_are_decoded_by_the_shared_header_seam() {
    let input = channel(
        "News",
        "https://example.invalid/live|Referer=https%3A%2F%2Fref.invalid%2Fa+b|Bearer=top%2Bsecret",
    );
    let result = plan(&input, "provider_mkv");
    let referer = result
        .argv
        .iter()
        .position(|arg| arg == "-referer")
        .unwrap();
    assert_eq!(result.argv[referer + 1], "https://ref.invalid/a b");
    let headers = result
        .argv
        .iter()
        .position(|arg| arg == "-headers")
        .unwrap();
    assert_eq!(
        result.argv[headers + 1],
        "Authorization: Bearer top+secret\r\n"
    );
}

#[test]
fn a_header_value_used_as_channel_name_is_not_announced() {
    let mut input = channel("Bearer private", "https://example.invalid/live");
    input
        .unknown_attributes
        .insert("http-authorization".into(), "Bearer private".into());
    let result = plan(&input, "provider_mkv");
    assert_eq!(result.display_label, "Recording");
    assert!(!format!("{result:?}").contains("private"));
}

#[test]
fn selected_track_does_not_add_a_second_map_to_provider_mka() {
    let input = channel("Radio", "http://example.invalid/live");
    let result = plan_recording(
        &input,
        Path::new("recordings"),
        "2026-11-01 12-34-56",
        RecordingOptions {
            format: "provider_mka",
            audio_track: Some(1),
            ..RecordingOptions::default()
        },
        &[],
        |_| false,
    )
    .unwrap();
    assert_eq!(
        result
            .argv
            .iter()
            .filter(|arg| arg.as_str() == "-map")
            .count(),
        1
    );
    assert!(result.argv.windows(2).any(|w| w == ["-map", "0:a?"]));
}

#[test]
fn truncation_keeps_reserved_and_trailing_dot_names_safe() {
    let reserved = format!("CON.{}", "a".repeat(116));
    assert!(sanitize_filename(&reserved).chars().count() <= 120);
    assert!(sanitize_filename(&reserved).ends_with('_'));
    let trailing_dot = format!("{}.", "a".repeat(119));
    assert!(!sanitize_filename(&trailing_dot).ends_with('.'));
}

#[test]
fn stats_duration_and_log_timestamps_have_stable_input_order() {
    let input = channel("News", "http://example.invalid/live");
    let result = plan_recording(
        &input,
        Path::new("recordings"),
        "2026-11-01 12-34",
        RecordingOptions {
            format: "provider_mkv",
            duration_seconds: Some(5.0),
            show_stats: true,
            log_datetime: true,
            ..RecordingOptions::default()
        },
        &[],
        |_| false,
    )
    .unwrap();
    assert_eq!(
        &result.argv[..9],
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "level+datetime+info",
            "-n",
            "-stats_period",
            "1",
            "-rw_timeout",
            "15000000"
        ]
    );
    assert!(result
        .argv
        .windows(4)
        .any(|w| w == ["-i", "http://example.invalid/live", "-t", "5.0"]));
}

#[test]
fn extra_headers_do_not_duplicate_canonical_auth() {
    let mut input = channel("News", "http://example.invalid/live");
    input
        .unknown_attributes
        .insert("http-authorization".into(), "Bearer correct".into());
    input
        .unknown_attributes
        .insert("http-headers".into(), "authorization: Bearer wrong".into());
    let result = plan(&input, "provider_mkv");
    let header_index = result
        .argv
        .iter()
        .position(|arg| arg == "-headers")
        .unwrap();
    assert_eq!(
        result.argv[header_index + 1],
        "Authorization: Bearer correct\r\n"
    );
}

#[test]
fn invalid_timestamp_is_rejected_before_path_planning() {
    let input = channel("News", "http://example.invalid/live");
    assert!(plan_recording(
        &input,
        Path::new("recordings"),
        "---",
        RecordingOptions::default(),
        &[],
        |_| false
    )
    .is_err());
}

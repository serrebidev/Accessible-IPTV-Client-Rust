use iptv_core::favorites::{FavoriteSet, ToggleOutcome};
use iptv_core::Channel;
use iptv_core::{favorites::channel_key, parse_m3u};
use std::collections::BTreeMap;

fn fixture_channels() -> Vec<(String, Channel)> {
    include_str!("fixtures/favorite-channels.tsv")
        .lines()
        .skip(1)
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            let unknown_attributes = [
                ("provider-type", fields[4]),
                ("provider-id", fields[5]),
                ("stream-id", fields[6]),
                ("stream_id", fields[7]),
                ("tvg_id", fields[9]),
                ("tvg_name", fields[11]),
                ("username", "ignored-user"),
                ("password", "ignored-password"),
                ("token", "ignored-token"),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect::<BTreeMap<_, _>>();
            (
                fields[0].into(),
                Channel {
                    name: fields[1].into(),
                    group: fields[2].into(),
                    url: fields[3].into(),
                    tvg_id: fields[8].into(),
                    tvg_name: fields[10].into(),
                    tvg_logo: "https://logo.invalid/user/password".into(),
                    unknown_attributes,
                },
            )
        })
        .collect()
}

#[test]
fn pinned_python_identity_normalization_toggle_and_filter_outcomes_match() {
    let fixtures = fixture_channels();
    let channels: Vec<_> = fixtures
        .iter()
        .map(|(_, channel)| channel.clone())
        .collect();
    let mut actual = Vec::new();
    for (label, channel) in &fixtures {
        actual.push(format!(
            "key\t{label}\t{}\t.",
            channel_key(channel).unwrap_or_default()
        ));
    }
    for line in include_str!("fixtures/favorite-stored.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split('\t').collect();
        let values = if fields[1] == "list" {
            &fields[2..fields.len() - 1]
        } else {
            &[]
        };
        let favorites =
            FavoriteSet::from_stored(values.iter().map(|value| value.strip_prefix("s:")));
        actual.push(
            [
                vec!["normalize", fields[0]],
                favorites.keys().iter().map(String::as_str).collect(),
                vec!["."],
            ]
            .concat()
            .join("\t"),
        );
    }
    let mut favorites = FavoriteSet::from_stored([Some("gone")]);
    for index in (0..channels.len()).cycle().take(channels.len() * 2) {
        let outcome = match favorites.toggle(&channels[index]) {
            ToggleOutcome::Added => "added",
            ToggleOutcome::Removed => "removed",
            ToggleOutcome::Unkeyable => "unkeyable",
        };
        actual.push(
            [
                vec!["toggle", &fixtures[index].0, outcome],
                favorites.keys().iter().map(String::as_str).collect(),
                vec!["."],
            ]
            .concat()
            .join("\t"),
        );
        let found = favorites.filter_channels(&channels);
        let labels = found
            .iter()
            .map(|channel| {
                let row = channels
                    .iter()
                    .position(|entry| std::ptr::eq(entry, *channel))
                    .unwrap();
                fixtures[row].0.as_str()
            })
            .collect::<Vec<_>>()
            .join(",");
        actual.push(format!("filter\t{}\t{labels}\t.", fixtures[index].0));
    }
    let expected: Vec<_> = include_str!("fixtures/python-favorites.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn stored_keys_are_normalized_before_toggle_and_filter_in_playlist_order() {
    let mut favorites = FavoriteSet::from_stored([
        Some("D"),
        Some(""),
        None,
        Some("  "),
        Some(" B "),
        Some("D"),
        Some("gone"),
    ]);
    assert_eq!(favorites.keys(), ["D", "B", "gone"]);
    let channels =
        parse_m3u("#EXTINF:-1,A\na\n#EXTINF:-1,B\nb\n#EXTINF:-1,C\nc\n#EXTINF:-1,D\nd\n");
    assert_eq!(
        favorites.filter_channels(&channels),
        [&channels[1], &channels[3]]
    );
    assert_eq!(favorites.toggle(&channels[1]), ToggleOutcome::Removed);
    assert_eq!(favorites.keys(), ["D", "gone"]);
    assert_eq!(favorites.toggle(&channels[0]), ToggleOutcome::Added);
    assert_eq!(favorites.keys(), ["D", "gone", "A"]);
    let nameless = &parse_m3u("https://user:secret@panel.invalid/live\n")[0];
    assert_eq!(favorites.toggle(nameless), ToggleOutcome::Unkeyable);
    assert_eq!(favorites.keys(), ["D", "gone", "A"]);
    assert!(!favorites.contains(nameless));
    assert!(favorites.contains(&channels[0]));
    assert!(!favorites.contains(&channels[1]));
    assert!(FavoriteSet::default().filter_channels(&channels).is_empty());
    assert!(favorites.filter_channels(&[]).is_empty());
}

#[test]
fn namesakes_share_a_favorite_but_escaped_names_cannot_impersonate_metadata() {
    let channels = parse_m3u(
        "#EXTINF:-1 group-title=UK,BBC One\na\n#EXTINF:-1 group-title=News,BBC One\nb\n#EXTINF:-1,ITV|1\nc\n#EXTINF:-1 tvg-id=ITV,1\nd\n",
    );
    let mut favorites = FavoriteSet::default();
    assert_eq!(favorites.toggle(&channels[0]), ToggleOutcome::Added);
    assert_eq!(
        favorites.filter_channels(&channels),
        [&channels[0], &channels[1]]
    );
    assert_eq!(favorites.toggle(&channels[1]), ToggleOutcome::Removed);
    assert!(favorites.filter_channels(&channels).is_empty());
    assert_eq!(channel_key(&channels[2]).as_deref(), Some("ITV\\|1"));
    assert_eq!(channel_key(&channels[3]).as_deref(), Some("ITV|1"));
    assert_eq!(favorites.toggle(&channels[2]), ToggleOutcome::Added);
    assert_eq!(favorites.filter_channels(&channels), [&channels[2]]);
    assert_eq!(favorites.toggle(&channels[3]), ToggleOutcome::Added);
    assert_eq!(
        favorites.filter_channels(&channels),
        [&channels[2], &channels[3]]
    );
}

#[test]
fn provider_identity_survives_credentials_and_stream_url_refresh() {
    let mut channel = parse_m3u(
        "#EXTINF:-1 provider-type=xtream provider-id=prov-1 stream-id=4242 tvg-id=bbc1.uk,BBC One HD\nhttp://panel.invalid/live/user/pass/4242.ts\n",
    ).remove(0);
    let expected = "xtream|prov-1|4242|bbc1.uk|BBC One HD";
    assert_eq!(channel_key(&channel).as_deref(), Some(expected));
    channel.url = "https://user:newpass@other.invalid/rotated?token=secret".into();
    assert_eq!(channel_key(&channel).as_deref(), Some(expected));
    channel
        .unknown_attributes
        .insert("provider-id".into(), "prov-2".into());
    assert_ne!(channel_key(&channel).as_deref(), Some(expected));
    channel
        .unknown_attributes
        .insert("provider-id".into(), "prov-1".into());
    channel
        .unknown_attributes
        .insert("stream-id".into(), "4243".into());
    assert_ne!(channel_key(&channel).as_deref(), Some(expected));
}

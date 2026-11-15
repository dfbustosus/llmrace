use llmrace::bench::{LaneSummary, RunMetrics};
use llmrace::score::{podium, score_lanes, Weights};

fn lane(name: &str, ttfb: f64, tps: f64, p95: f64) -> LaneSummary {
    lane_with_model(name, "m", ttfb, tps, p95)
}

fn lane_with_model(name: &str, model: &str, ttfb: f64, tps: f64, p95: f64) -> LaneSummary {
    let gen_s = (p95 - ttfb) / 1000.0;
    LaneSummary {
        name: name.into(),
        model: model.into(),
        runs: vec![RunMetrics {
            total_ms: p95,
            ttfb_ms: ttfb,
            tokens: (tps * gen_s).round().max(1.0) as u64,
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    }
}

#[test]
fn winner_scores_hundred() {
    let lanes = vec![
        lane("fast", 10.0, 200.0, 100.0),
        lane("slow", 40.0, 50.0, 400.0),
    ];
    let scored = score_lanes(&lanes, &Weights::default());
    assert_eq!(scored[0].name, "fast");
    assert!((scored[0].score - 100.0).abs() < 1e-9);
    assert!(scored[1].score < 100.0);
}

#[test]
fn single_lane_not_scored() {
    let lanes = vec![lane("solo", 10.0, 200.0, 100.0)];
    assert!(score_lanes(&lanes, &Weights::default()).is_empty());
}

#[test]
fn failed_lane_ignored() {
    let mut failed = lane("dead", 1.0, 1.0, 1.0);
    failed.error = Some("boom".into());
    let lanes = vec![lane("ok", 10.0, 200.0, 100.0), failed];
    assert!(score_lanes(&lanes, &Weights::default()).is_empty());
}

#[test]
fn weights_parse_defaults_and_errors() {
    assert_eq!(Weights::parse("").unwrap(), Weights::default());
    assert_eq!(Weights::parse("tps=60").unwrap().tps, 60.0);
    assert!(Weights::parse("ttfb=0,tps=0,p95=0").is_err());
    assert!(Weights::parse("ttfb").is_err());
    assert!(Weights::parse("bogus=1").is_err());
}

#[test]
fn podium_renders_medals() {
    let lanes = vec![
        lane("fast", 10.0, 200.0, 100.0),
        lane("mid", 20.0, 100.0, 200.0),
        lane("slow", 40.0, 50.0, 400.0),
    ];
    let scored = score_lanes(&lanes, &Weights::default());
    let text = podium(&scored, &Weights::default());
    assert!(text.contains("🥇 fast"));
    assert!(text.contains("🥉 slow"));
}

#[test]
fn duplicate_providers_label_with_model() {
    let lanes = vec![
        lane_with_model("openai", "gpt-a", 10.0, 200.0, 100.0),
        lane_with_model("openai", "gpt-b", 20.0, 100.0, 200.0),
    ];
    let scored = score_lanes(&lanes, &Weights::default());
    assert!(
        scored[0].name.starts_with("openai/gpt-"),
        "{:?}",
        scored[0].name
    );
    assert!(scored.iter().all(|s| s.name.contains("gpt-")), "{scored:?}");
}

#[test]
fn sweep_podiums_group_by_variant() {
    let mut short_a = lane_with_model("openai", "a", 10.0, 200.0, 100.0);
    short_a.variant = Some("short".into());
    let mut short_b = lane_with_model("groq", "b", 20.0, 100.0, 200.0);
    short_b.variant = Some("short".into());
    let mut json_a = lane_with_model("openai", "a", 400.0, 20.0, 900.0);
    json_a.variant = Some("json".into());

    let groups = score_lanes_groups_helper(vec![short_a, short_b, json_a]);
    // two groups: short (2 lanes) and json (1 lane, too few to score)
    assert_eq!(groups.len(), 2);
    let (label, scored) = &groups[0];
    assert_eq!(label.as_deref(), Some("short"));
    assert_eq!(scored.len(), 2);

    let text = llmrace::score::podiums(&groups, &Weights::default());
    assert!(text.contains("variant `short`"), "{text}");
}

fn score_lanes_groups_helper(
    lanes: Vec<llmrace::bench::LaneSummary>,
) -> Vec<(Option<String>, Vec<llmrace::score::Scored>)> {
    llmrace::score::score_groups(&lanes, &Weights::default())
}

/// A lane on a priced provider (pricing keys on provider name + model).
fn priced_lane(name: &str, tps: f64, p95: f64) -> LaneSummary {
    let (model, ttfb) = if name == "openai" {
        ("gpt-4o-mini", 100.0)
    } else {
        ("gemini-2.0-flash", 50.0)
    };
    let gen_s = (p95 - ttfb) / 1000.0;
    LaneSummary {
        name: name.into(),
        model: model.into(),
        runs: vec![RunMetrics {
            total_ms: p95,
            ttfb_ms: ttfb,
            tokens: (tps * gen_s).round().max(1.0) as u64,
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    }
}

#[test]
fn cost_weight_changes_ranking() {
    // two priced providers with identical throughput: the cheaper must win
    // when cost has weight.
    let a = priced_lane("openai", 200.0, 1100.0);
    let b = priced_lane("gemini", 200.0, 1050.0);
    let with_cost = Weights {
        ttfb: 0.0,
        tps: 50.0,
        p95: 0.0,
        cost: 50.0,
    };
    let scored = score_lanes(&[a.clone(), b.clone()], &with_cost);
    assert_eq!(
        scored[0].name, "gemini",
        "cheaper lane must lead: {scored:?}"
    );

    // weight parsing accepts cost and rejects junk
    assert_eq!(Weights::parse("cost=40").unwrap().cost, 40.0);
    assert!(Weights::parse("bogus=1").is_err());
}

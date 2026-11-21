//! JSON Schema for `race.json` reports, so CI consumers can validate
//! reports with standard tooling (`ajv`, `jsonschema`, editor support).

/// The JSON Schema (draft 2020-12) describing `llmrace --json` output.
#[must_use]
pub fn race_report_schema() -> String {
    r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://github.com/dfbustosus/llmrace/schema/race-report.json",
  "title": "llmrace race report",
  "type": "object",
  "required": ["tool", "version", "prompt", "runs", "max_tokens", "providers"],
  "properties": {
    "tool": {"const": "llmrace"},
    "version": {"type": "string", "pattern": "^\\d+\\.\\d+\\.\\d+$"},
    "region": {"type": ["string", "null"]},
    "prompt": {"type": "string"},
    "runs": {"type": "integer", "minimum": 1},
    "max_tokens": {"type": "integer", "minimum": 1},
    "sampling": {
      "type": "object",
      "description": "Present only when the race pinned sampling. Omitted controls were left at the provider default.",
      "properties": {
        "temperature": {"type": "number", "minimum": 0, "maximum": 2},
        "top_p": {"type": "number", "minimum": 0, "maximum": 1},
        "seed": {"type": "integer", "minimum": 0},
        "system": {"type": "string"}
      }
    },
    "significance": {
      "type": ["object", "null"],
      "properties": {
        "a": {"type": "string"},
        "b": {"type": "string"},
        "delta_ms": {"type": "number"},
        "p_value": {"type": "number", "minimum": 0, "maximum": 1},
        "significant": {"type": "boolean"}
      }
    },
    "providers": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["provider", "model", "successful"],
        "properties": {
          "provider": {"type": "string"},
          "model": {"type": "string"},
          "variant": {"type": ["string", "null"]},
          "concurrency": {"type": "integer", "minimum": 1},
          "successful": {"type": "boolean"},
          "error": {"type": ["string", "null"]},
          "measured_rounds": {"type": "integer", "minimum": 0},
          "failed_rounds": {"type": "integer", "minimum": 0},
          "failure_rate": {"type": ["number", "null"], "minimum": 0, "maximum": 1},
          "retry_attempts": {"type": "integer", "minimum": 0},
          "goodput_tokens_per_sec": {"type": "number", "minimum": 0},
          "failures": {
            "type": "array",
            "items": {
              "type": "object",
              "required": ["elapsed_ms", "class", "attempts", "message"],
              "properties": {
                "elapsed_ms": {"type": "number", "minimum": 0},
                "class": {"type": "string"},
                "attempts": {"type": "integer", "minimum": 1},
                "message": {"type": "string"}
              }
            }
          },
          "runs": {
            "type": "array",
            "items": {
              "type": "object",
              "required": ["total_ms", "ttfb_ms", "tokens"],
              "properties": {
                "total_ms": {"type": "number", "minimum": 0},
                "ttfb_ms": {"type": "number", "minimum": 0},
                "tokens": {"type": "integer", "minimum": 0},
                "chars": {"type": "integer", "minimum": 0},
                "prompt_tokens": {"type": "integer", "minimum": 0},
                "cache_read_tokens": {"type": "integer", "minimum": 0},
                "cache_write_tokens": {"type": "integer", "minimum": 0},
                "cache_write_1h_tokens": {"type": "integer", "minimum": 0},
                "tokens_per_sec": {"type": "number", "minimum": 0}
              }
            }
          },
          "mean_total_ms": {"type": "number"},
          "median_total_ms": {"type": "number"},
          "p95_total_ms": {"type": "number"},
          "ci95_total_ms": {
            "type": ["array", "number"],
            "items": {"type": "number"},
            "minItems": 2, "maxItems": 2
          },
          "mean_ttfb_ms": {"type": "number"},
          "ttft_ms": {"type": "number"},
          "ttfb_p50_ms": {"type": ["number", "null"]},
          "ttfb_p95_ms": {"type": ["number", "null"]},
          "tpot_p50_ms": {"type": ["number", "null"]},
          "tpot_p95_ms": {"type": ["number", "null"]},
          "mean_tokens_per_sec": {"type": "number"},
          "outlier_runs": {"type": "array", "items": {"type": "integer"}},
          "cost_per_run_usd": {"type": ["number", "null"]},
          "prompt_tokens": {"type": ["number", "null"]},
          "prompt_tokens_measured": {"type": "boolean"},
          "cache_read_tokens": {"type": ["number", "null"]},
          "cache_write_tokens": {"type": ["number", "null"]},
          "cache_write_1h_tokens": {"type": ["number", "null"]},
          "cache_discount_applied": {"type": "boolean"},
          "server_ms": {"type": ["number", "null"]},
          "network_overhead_ms": {"type": ["number", "null"]},
          "itl_p50_ms": {"type": ["number", "null"]},
          "itl_p95_ms": {"type": ["number", "null"]},
          "stutters": {"type": "integer", "minimum": 0},
          "visible_tokens": {"type": "integer", "minimum": 0},
          "reasoning_tokens": {"type": "integer", "minimum": 0},
          "thinking_ms_estimate": {"type": ["number", "null"]},
          "request_id": {"type": ["string", "null"]},
          "tool_validity": {
            "type": ["array", "null"],
            "items": {"type": "integer", "minimum": 0},
            "minItems": 2, "maxItems": 2
          }
        }
      }
    }
  }
}"#
    .to_string()
}

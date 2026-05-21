use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub prompt_cache_hit_tokens: u64,
    #[serde(default)]
    pub prompt_cache_miss_tokens: u64,
}

impl ModelUsage {
    pub fn cache_hit_tokens(&self) -> u64 {
        self.cache_read_input_tokens + self.prompt_cache_hit_tokens
    }

    pub fn cache_miss_tokens(&self) -> u64 {
        self.cache_creation_input_tokens + self.prompt_cache_miss_tokens
    }

    pub fn has_cache_telemetry(&self) -> bool {
        self.cache_hit_tokens() > 0 || self.cache_miss_tokens() > 0
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptCacheStats {
    #[serde(default)]
    pub total_model_calls: u64,
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    #[serde(default)]
    pub total_cache_creation_input_tokens: u64,
    #[serde(default)]
    pub total_cache_read_input_tokens: u64,
    #[serde(default)]
    pub total_prompt_cache_hit_tokens: u64,
    #[serde(default)]
    pub total_prompt_cache_miss_tokens: u64,
    #[serde(default)]
    pub total_local_cache_hits: u64,
    #[serde(default)]
    pub last_local_cache_hit: bool,
    #[serde(default)]
    pub last_usage: ModelUsage,
}

impl PromptCacheStats {
    pub fn record_usage(&mut self, usage: &ModelUsage) {
        self.total_model_calls += 1;
        self.total_input_tokens += usage.input_tokens + usage.prompt_tokens;
        self.total_output_tokens += usage.output_tokens + usage.completion_tokens;
        self.total_cache_creation_input_tokens += usage.cache_creation_input_tokens;
        self.total_cache_read_input_tokens += usage.cache_read_input_tokens;
        self.total_prompt_cache_hit_tokens += usage.prompt_cache_hit_tokens;
        self.total_prompt_cache_miss_tokens += usage.prompt_cache_miss_tokens;
        self.last_local_cache_hit = false;
        self.last_usage = usage.clone();
    }

    pub fn record_local_cache_hit(&mut self) {
        self.total_local_cache_hits += 1;
        self.last_local_cache_hit = true;
    }

    pub fn merge(&mut self, other: &Self) {
        self.total_model_calls += other.total_model_calls;
        self.total_input_tokens += other.total_input_tokens;
        self.total_output_tokens += other.total_output_tokens;
        self.total_cache_creation_input_tokens += other.total_cache_creation_input_tokens;
        self.total_cache_read_input_tokens += other.total_cache_read_input_tokens;
        self.total_prompt_cache_hit_tokens += other.total_prompt_cache_hit_tokens;
        self.total_prompt_cache_miss_tokens += other.total_prompt_cache_miss_tokens;
        self.total_local_cache_hits += other.total_local_cache_hits;
        self.last_local_cache_hit = other.last_local_cache_hit;
        self.last_usage = other.last_usage.clone();
    }

    pub fn last_hit_tokens(&self) -> u64 {
        self.last_usage.cache_hit_tokens()
    }

    pub fn last_miss_tokens(&self) -> u64 {
        self.last_usage.cache_miss_tokens()
    }

    pub fn total_hit_tokens(&self) -> u64 {
        self.total_cache_read_input_tokens + self.total_prompt_cache_hit_tokens
    }

    pub fn total_miss_tokens(&self) -> u64 {
        self.total_cache_creation_input_tokens + self.total_prompt_cache_miss_tokens
    }

    pub fn total_hit_rate(&self) -> Option<f64> {
        ratio(self.total_hit_tokens(), self.total_miss_tokens())
    }

    pub fn summary_line(&self) -> String {
        let total_rate = self
            .total_hit_rate()
            .map(format_percent)
            .unwrap_or_else(|| "n/a".to_string());
        let local_summary = if self.total_local_cache_hits > 0 {
            format!(
                " local_cache_hits={} last_local_cache_hit={}",
                self.total_local_cache_hits, self.last_local_cache_hit
            )
        } else {
            String::new()
        };

        if self.last_usage.has_cache_telemetry()
            || self.total_hit_tokens() > 0
            || self.total_miss_tokens() > 0
            || self.total_local_cache_hits > 0
        {
            format!(
                "info: prompt cache stats: total_hit_tokens={} total_miss_tokens={} total_hit_rate={} total_model_calls={}{}",
                self.total_hit_tokens(),
                self.total_miss_tokens(),
                total_rate,
                self.total_model_calls,
                local_summary
            )
        } else {
            format!(
                "info: prompt cache stats: model_calls={} (no cache telemetry returned yet)",
                self.total_model_calls
            )
        }
    }
}

fn ratio(hit_tokens: u64, miss_tokens: u64) -> Option<f64> {
    let total = hit_tokens + miss_tokens;
    if total == 0 {
        None
    } else {
        Some(hit_tokens as f64 / total as f64)
    }
}

fn format_percent(value: f64) -> String {
    format!("{:.1}%", value * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_provider_hit_rate() {
        let mut stats = PromptCacheStats::default();
        stats.record_usage(&ModelUsage {
            cache_read_input_tokens: 75,
            cache_creation_input_tokens: 25,
            ..Default::default()
        });
        assert_eq!(stats.total_hit_rate(), Some(0.75));
    }
}

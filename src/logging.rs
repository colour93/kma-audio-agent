use serde_json::Value;

pub fn redact_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let key = key.to_ascii_lowercase();
                if ["token", "authorization", "cookie", "password", "secret"]
                    .iter()
                    .any(|sensitive| key.contains(sensitive))
                {
                    *value = Value::String("[REDACTED]".to_owned());
                } else {
                    redact_json(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        Value::String(text) => {
            if let Ok(mut url) = url::Url::parse(text) {
                let mut changed = false;
                let pairs = url
                    .query_pairs()
                    .map(|(key, value)| {
                        if key.to_ascii_lowercase().contains("token") {
                            changed = true;
                            (key.into_owned(), "[REDACTED]".to_owned())
                        } else {
                            (key.into_owned(), value.into_owned())
                        }
                    })
                    .collect::<Vec<_>>();
                if changed {
                    url.query_pairs_mut().clear().extend_pairs(pairs);
                    *text = url.into();
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_tokens_recursively_and_in_urls() {
        let mut value = serde_json::json!({
            "token": "top-secret",
            "nested": { "authorization": "Bearer secret" },
            "url": "http://server/media?token=secret&asset=1"
        });
        redact_json(&mut value);
        assert_eq!(value["token"], "[REDACTED]");
        assert_eq!(value["nested"]["authorization"], "[REDACTED]");
        assert!(!value["url"].as_str().unwrap().contains("secret"));
    }
}

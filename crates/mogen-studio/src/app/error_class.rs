//! Convert raw [`GeminiError`]s into a structured [`LlmErrorInfo`] so the UI
//! can swap in a class-specific affordance ("Open Settings", "Retry") instead
//! of dumping the error's `Display` output verbatim.

use mogen_llm::gemini::GeminiError;

use super::types::{LlmErrorClass, LlmErrorInfo};

pub(super) fn classify(err: &GeminiError) -> LlmErrorInfo {
    match err {
        GeminiError::MissingApiKey => LlmErrorInfo {
            headline: "No Gemini API key".into(),
            detail:
                "Paste a key in Edit → Preferences… or export GEMINI_API_KEY before trying again."
                    .into(),
            class: LlmErrorClass::MissingKey,
            retryable: false,
        },
        GeminiError::Transport(e) => LlmErrorInfo {
            headline: "Network error".into(),
            detail: format!("{e}. Check your connection and try again."),
            class: LlmErrorClass::Network,
            retryable: true,
        },
        GeminiError::Api { status, message } => classify_api(*status, message),
        GeminiError::EmptyResponse => LlmErrorInfo {
            headline: "Model returned an empty response".into(),
            detail:
                "Gemini produced no text. This usually means the prompt was blocked by a safety \
                 or recitation filter — try rephrasing or simplifying the request."
                    .into(),
            class: LlmErrorClass::ContentBlocked,
            retryable: true,
        },
        GeminiError::BudgetExceeded { used, budget } => LlmErrorInfo {
            headline: "Token budget exceeded".into(),
            detail: format!(
                "Used {used} tokens but the per-call budget is {budget}. Raise the cap in \
                 Preferences → Advanced or shorten the prompt."
            ),
            class: LlmErrorClass::BadRequest,
            retryable: false,
        },
        GeminiError::InvalidResponse(msg) => {
            let is_recitation = msg.contains("RECITATION") || msg.contains("IMAGE_RECITATION");
            let is_safety = msg.contains("SAFETY") || msg.contains("BLOCKED");
            if is_recitation {
                LlmErrorInfo {
                    headline: "Model declined to produce content".into(),
                    detail:
                        "Gemini's recitation filter rejected every attempt. Try a different \
                         style description or rename/reword the material so the prompt does \
                         not resemble training data."
                            .into(),
                    class: LlmErrorClass::ContentBlocked,
                    retryable: true,
                }
            } else if is_safety {
                LlmErrorInfo {
                    headline: "Prompt blocked by safety filters".into(),
                    detail: msg.clone(),
                    class: LlmErrorClass::ContentBlocked,
                    retryable: false,
                }
            } else {
                LlmErrorInfo {
                    headline: "Invalid response from Gemini".into(),
                    detail: msg.clone(),
                    class: LlmErrorClass::Other,
                    retryable: true,
                }
            }
        }
    }
}

fn classify_api(status: u16, message: &str) -> LlmErrorInfo {
    let msg_lower = message.to_ascii_lowercase();
    match status {
        400 => LlmErrorInfo {
            headline: "Bad request".into(),
            detail: format!("Gemini rejected the request: {message}"),
            class: LlmErrorClass::BadRequest,
            retryable: false,
        },
        401 | 403 if msg_lower.contains("api key") || msg_lower.contains("authentication") => {
            LlmErrorInfo {
                headline: "API key rejected".into(),
                detail:
                    "Gemini refused the request as unauthenticated. Open Preferences… and paste \
                     a valid key, or verify GEMINI_API_KEY is set."
                        .into(),
                class: LlmErrorClass::InvalidKey,
                retryable: false,
            }
        }
        403 if msg_lower.contains("quota") => LlmErrorInfo {
            headline: "Quota exceeded".into(),
            detail:
                "Your project has hit its daily or per-minute Gemini quota. Wait for the quota to \
                 reset or request a higher limit in the Google Cloud console."
                    .into(),
            class: LlmErrorClass::QuotaExceeded,
            retryable: false,
        },
        403 => LlmErrorInfo {
            headline: "Request forbidden".into(),
            detail: format!("Gemini returned 403: {message}"),
            class: LlmErrorClass::InvalidKey,
            retryable: false,
        },
        404 => LlmErrorInfo {
            headline: "Model not found".into(),
            detail: format!(
                "Gemini has no model matching the configured name. Check Preferences → \
                 Advanced → Model. ({message})"
            ),
            class: LlmErrorClass::BadRequest,
            retryable: false,
        },
        429 => LlmErrorInfo {
            headline: "Rate limited".into(),
            detail:
                "Too many requests in a short window. Wait a few seconds and try again, or slow \
                 the cadence if you're hitting this repeatedly."
                    .into(),
            class: LlmErrorClass::RateLimited,
            retryable: true,
        },
        s if (500..600).contains(&s) => LlmErrorInfo {
            headline: "Gemini server error".into(),
            detail: format!("({s}) {message}. This is usually transient — try again."),
            class: LlmErrorClass::ServerError,
            retryable: true,
        },
        s => LlmErrorInfo {
            headline: format!("API error {s}"),
            detail: message.to_string(),
            class: LlmErrorClass::Other,
            retryable: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_401_hints_open_settings() {
        let err = GeminiError::Api {
            status: 401,
            message: "API key not valid".into(),
        };
        let info = classify(&err);
        assert_eq!(info.class, LlmErrorClass::InvalidKey);
        assert!(!info.retryable);
    }

    #[test]
    fn classify_429_is_retryable() {
        let err = GeminiError::Api { status: 429, message: "rate".into() };
        let info = classify(&err);
        assert!(matches!(info.class, LlmErrorClass::RateLimited));
        assert!(info.retryable);
    }

    #[test]
    fn classify_500_is_retryable() {
        let err = GeminiError::Api { status: 503, message: "down".into() };
        let info = classify(&err);
        assert!(matches!(info.class, LlmErrorClass::ServerError));
        assert!(info.retryable);
    }

    #[test]
    fn classify_missing_key_not_retryable() {
        let info = classify(&GeminiError::MissingApiKey);
        assert!(!info.retryable);
        assert_eq!(info.class, LlmErrorClass::MissingKey);
    }

    #[test]
    fn classify_recitation_as_blocked() {
        let err = GeminiError::InvalidResponse("no image returned (finishReason: IMAGE_RECITATION)".into());
        let info = classify(&err);
        assert_eq!(info.class, LlmErrorClass::ContentBlocked);
    }
}

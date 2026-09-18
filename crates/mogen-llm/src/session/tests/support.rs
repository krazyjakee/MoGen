//! Shared fixtures for the `session::tests` submodules: a baseline source,
//! revision/response helpers, and a scripted [`SessionRenderer`].
use super::super::*;
use crate::{GenerateResponse, ImageInput, Usage};

pub(super) const ORIGINAL:&str="material \"wood\" (color=[0.5,0.3,0.1])\nscene { box \"seat\" (size=[1,0.2,1],mat=\"wood\") box \"arm\" (size=[0.1,0.1,1],pos=[0.45,0.1,0],mat=\"wood\") }";
pub(super) fn rev(source: &str) -> String {
    revision(source, &Default::default())
}
pub(super) fn response(text: impl Into<String>) -> GenerateResponse {
    GenerateResponse {
        text: text.into(),
        usage: Usage {
            prompt_tokens: 10,
            response_tokens: 10,
            total_tokens: 20,
            cached_tokens: 0,
        },
    }
}
pub(super) struct Renderer {
    pub(super) seen: Vec<(String, View)>,
    pub(super) fail: bool,
}
impl SessionRenderer for Renderer {
    fn render(&mut self, _source: &str, revision: &str, view: View) -> anyhow::Result<ImageInput> {
        if self.fail {
            anyhow::bail!("fixture render failure");
        }
        self.seen.push((revision.into(), view));
        Ok(ImageInput {
            mime_type: "image/png".into(),
            data: format!("{revision}-{}", view.label()).into_bytes(),
        })
    }
}

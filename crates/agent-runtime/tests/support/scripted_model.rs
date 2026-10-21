use std::{collections::VecDeque, convert::Infallible, sync::Mutex};

use agent_runtime::{Model, ModelRequest, ModelResponse};

/// Returns each scripted response once and records requests for assertions.
///
/// Exhausting the script is an unexpected request and fails the test.
pub struct ScriptedModel {
    responses: Mutex<VecDeque<ModelResponse>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    pub fn new(responses: impl IntoIterator<Item = ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Model for ScriptedModel {
    type Error = Infallible;

    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, Self::Error> {
        self.requests.lock().unwrap().push(request);
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected model request after the response script was exhausted"))
    }
}

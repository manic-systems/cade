use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
pub enum HookType {
    LoadPre,
    LoadPost,
    UnloadPre,
    UnloadPost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InnerHook {
    pub content: String,
    pub kind: HookType,
}

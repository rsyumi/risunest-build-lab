use super::contract::VersionToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeadObservation {
    pub commit_id: String,
    pub authenticated_body_hash: String,
    pub version: Option<VersionToken>,
}

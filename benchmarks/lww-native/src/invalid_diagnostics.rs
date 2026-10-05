use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;

#[derive(Clone,Debug,Serialize,Deserialize,PartialEq,Eq)]
pub struct JsonByteNode {
    pub kind:String,
    pub encoded_bytes:u64,
    pub own_bytes:u64,
    pub children:u64,
}

#[derive(Clone,Debug,Serialize,Deserialize,PartialEq,Eq)]
pub struct JsonByteStructure {
    pub input_bytes:u64,
    pub nodes:BTreeMap<String,JsonByteNode>,
}

#[derive(Clone,Debug,Serialize,Deserialize,Default)]
pub struct ReceiveIntentEvidence {
    pub observed_calls:u64,
    pub observed_bytes:u64,
    pub captured_calls:u64,
    pub captured_bytes:u64,
    pub complete:bool,
    pub errors:Vec<String>,
    pub inputs:Vec<JsonByteStructure>,
}

pub type CommitIntentEvidence=ReceiveIntentEvidence;

impl ReceiveIntentEvidence {
    pub fn finish(&mut self) {
        self.finish_for("receive-intent");
    }
    pub fn finish_for(&mut self,domain:&str) {
        if self.captured_calls!=self.observed_calls || self.captured_bytes!=self.observed_bytes {
            self.errors.push(format!("captured {domain} calls/bytes differ from actual SHA domain"));
        }
        if self.inputs.len() as u64!=self.captured_calls {
            self.errors.push("not every captured input has a verified partition".into());
        }
        self.complete=self.errors.is_empty();
    }
}

struct Parser<'a> {input:&'a [u8],position:usize,nodes:BTreeMap<String,JsonByteNode>}

impl Parser<'_> {
    fn whitespace(&mut self) {
        while self.input.get(self.position).is_some_and(u8::is_ascii_whitespace) {self.position+=1;}
    }
    fn expect(&mut self,byte:u8) -> Result<(),String> {
        if self.input.get(self.position)!=Some(&byte) {return Err("JSON token span mismatch".into());}
        self.position+=1;
        Ok(())
    }
    fn string(&mut self) -> Result<(),String> {
        self.expect(b'"')?;
        loop {
            match self.input.get(self.position) {
                Some(b'"') => {self.position+=1;return Ok(());},
                Some(b'\\') => {self.position+=2;},
                Some(_) => {self.position+=1;},
                None => return Err("unterminated JSON string span".into()),
            }
        }
    }
    fn value(&mut self,path:String) -> Result<u64,String> {
        let start=self.position;
        self.whitespace();
        let first=*self.input.get(self.position).ok_or("missing JSON value")?;
        let mut child_bytes=0u64;
        let mut children=0u64;
        let kind=match first {
            b'{' => {
                self.position+=1;
                self.whitespace();
                if self.input.get(self.position)!=Some(&b'}') {
                    loop {
                        self.whitespace();
                        let key_start=self.position;
                        self.string()?;
                        let key:String=serde_json::from_slice(&self.input[key_start..self.position]).map_err(|e|e.to_string())?;
                        self.whitespace();self.expect(b':')?;
                        let key=key.replace('~',"~0").replace('/',"~1");
                        child_bytes=child_bytes.checked_add(self.value(format!("{path}/{key}"))?).ok_or("span byte overflow")?;
                        children+=1;
                        self.whitespace();
                        if self.input.get(self.position)==Some(&b'}') {break;}
                        self.expect(b',')?;
                    }
                }
                self.expect(b'}')?;
                "object"
            },
            b'[' => {
                self.position+=1;self.whitespace();
                if self.input.get(self.position)!=Some(&b']') {
                    loop {
                        child_bytes=child_bytes.checked_add(self.value(format!("{path}/{children}"))?).ok_or("span byte overflow")?;
                        children+=1;self.whitespace();
                        if self.input.get(self.position)==Some(&b']') {break;}
                        self.expect(b',')?;
                    }
                }
                self.expect(b']')?;
                "array"
            },
            b'"' => {self.string()?;"string"},
            _ => {
                while self.input.get(self.position).is_some_and(|b|!b.is_ascii_whitespace() && ![b',',b']',b'}'].contains(b)) {
                    self.position+=1;
                }
                match first {b't' | b'f'=>"boolean",b'n'=>"null",_=>"number"}
            },
        };
        let encoded_bytes=(self.position-start) as u64;
        let own_bytes=encoded_bytes.checked_sub(child_bytes).ok_or("overlapping JSON spans")?;
        if self.nodes.insert(path,JsonByteNode {kind:kind.into(),encoded_bytes,own_bytes,children}).is_some() {
            return Err("duplicate JSON field span".into());
        }
        Ok(encoded_bytes)
    }
}

pub fn exact_json_structure(input:&[u8],typed_reserialization:&[u8]) -> Result<JsonByteStructure,String> {
    if input!=typed_reserialization {return Err("actual SHA input failed exact typed roundtrip".into());}
    let _:serde_json::Value=serde_json::from_slice(input).map_err(|e|e.to_string())?;
    let mut parser=Parser {input,position:0,nodes:BTreeMap::new()};
    parser.value(String::new())?;
    let end=parser.position;
    parser.whitespace();
    if parser.position!=input.len() {return Err("unconsumed JSON SHA input bytes".into());}
    let trailing=(parser.position-end) as u64;
    let root=parser.nodes.get_mut("").ok_or("JSON root span missing")?;
    root.encoded_bytes+=trailing;root.own_bytes+=trailing;
    let sum=parser.nodes.values().try_fold(0u64,|sum,node|sum.checked_add(node.own_bytes).ok_or("span sum overflow"))?;
    if sum!=input.len() as u64 {return Err("SHA input partition does not conserve exact bytes".into());}
    Ok(JsonByteStructure {input_bytes:input.len() as u64,nodes:parser.nodes})
}

#[derive(Clone,Debug,Serialize,Deserialize,PartialEq,Eq)]
pub struct JsonByteDelta {
    pub path:String,
    pub baseline_bytes:u64,
    pub trial_bytes:u64,
    pub category:String,
}

pub fn byte_deltas(a:&JsonByteStructure,b:&JsonByteStructure) -> Vec<JsonByteDelta> {
    let paths=a.nodes.keys().chain(b.nodes.keys()).collect::<std::collections::BTreeSet<_>>();
    paths.into_iter().filter_map(|path| {
        let baseline_bytes=a.nodes.get(path).map_or(0,|node|node.own_bytes);
        let trial_bytes=b.nodes.get(path).map_or(0,|node|node.own_bytes);
        if baseline_bytes==trial_bytes {return None;}
        let category=if ["/changes","/commit","/aliases"].iter().any(|prefix|
            path==*prefix || path.starts_with(&format!("{prefix}/"))) {"protected-change-input"}
            else if path=="/progress" || path.starts_with("/progress/") || ["/bindingAuthority","/requestId","/admittedTimeUpperMs"].contains(&path.as_str()) {
                "transport-control-candidate"
            } else {"framing-or-unknown"};
        Some(JsonByteDelta {path:path.clone(),baseline_bytes,trial_bytes,category:category.into()})
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn missing_input_and_count_or_byte_mismatch_are_incomplete() {
        let mut evidence=ReceiveIntentEvidence {observed_calls:1,observed_bytes:2,..Default::default()};
        evidence.finish();
        assert!(!evidence.complete);
        let mut evidence=ReceiveIntentEvidence {observed_calls:1,observed_bytes:2,captured_calls:1,captured_bytes:2,..Default::default()};
        evidence.finish();
        assert!(!evidence.complete);
        let mut evidence=ReceiveIntentEvidence::default();
        evidence.finish();
        assert!(evidence.complete);
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn original_spans_preserve_field_order_escapes_numbers_and_partition_every_byte() {
        let input=br#" {"z":[1.0,"a\\\"b"],"progress":{"cursor":"99"},"a":true} "#;
        let structure=exact_json_structure(input,input).unwrap();
        assert_eq!(structure.nodes.values().map(|node|node.own_bytes).sum::<u64>(),input.len() as u64);
        assert_eq!(structure.nodes["/z/0"].encoded_bytes,3);
        let reordered=br#" {"a":true,"progress":{"cursor":"99"},"z":[1.0,"a\\\"b"]} "#;
        assert!(exact_json_structure(input,reordered).is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn cursor_digit_growth_is_separate_from_protected_value_growth() {
        let a=br#"{"changes":[{"value":"same"}],"progress":{"cursor":"99"}}"#;
        let b=br#"{"changes":[{"value":"same"}],"progress":{"cursor":"100"}}"#;
        let deltas=byte_deltas(&exact_json_structure(a,a).unwrap(),&exact_json_structure(b,b).unwrap());
        assert_eq!(deltas.len(),1);
        assert_eq!(deltas[0].path,"/progress/cursor");
        assert_eq!(deltas[0].trial_bytes-deltas[0].baseline_bytes,1);
        let changed=br#"{"changes":[{"value":"different"}],"progress":{"cursor":"100"}}"#;
        assert!(byte_deltas(&exact_json_structure(a,a).unwrap(),&exact_json_structure(changed,changed).unwrap())
            .iter().any(|delta|delta.category=="protected-change-input"));
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn commit_revision_and_alias_changes_remain_protected_pending_evidence() {
        let a=br#"{"kind":"commit","commit":{"expectedRevision":9},"aliases":[]}"#;
        let b=br#"{"kind":"commit","commit":{"expectedRevision":10},"aliases":[]}"#;
        let deltas=byte_deltas(&exact_json_structure(a,a).unwrap(),&exact_json_structure(b,b).unwrap());
        assert_eq!(deltas.len(),1);
        assert_eq!(deltas[0].path,"/commit/expectedRevision");
        assert_eq!(deltas[0].category,"protected-change-input");
        let changed=br#"{"kind":"commit","commit":{"expectedRevision":10},"aliases":[{"key":"new"}]}"#;
        assert!(byte_deltas(&exact_json_structure(b,b).unwrap(),&exact_json_structure(changed,changed).unwrap())
            .iter().filter(|delta|delta.path.starts_with("/aliases")).all(|delta|delta.category=="protected-change-input"));
    }
}

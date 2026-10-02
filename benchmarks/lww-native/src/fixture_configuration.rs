use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use std::net::IpAddr;

#[derive(Clone,Debug,Default,Serialize,Deserialize)]
pub struct NativeFixtureConfiguration {pub local_sse:Option<LocalSseEndpoint>}

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct LocalSseEndpoint {pub address:IpAddr,pub port:u16}

impl LocalSseEndpoint {
    pub fn url(&self)->Result<String,String> {
        let local=match self.address {IpAddr::V4(ip)=>ip.is_loopback() || ip.is_private(),
            IpAddr::V6(ip)=>ip.is_loopback() || ip.is_unique_local()};
        if !local || self.port==0 {return Err("synthetic SSE requires an explicit loopback/private address and nonzero port".into());}
        let host=match self.address {IpAddr::V4(ip)=>ip.to_string(),IpAddr::V6(ip)=>format!("[{ip}]")};
        Ok(format!("http://{host}:{}/v1/chat/completions",self.port))
    }
}

impl NativeFixtureConfiguration {
    pub fn initial_values(&self)->Result<(Value,Vec<Value>),String> {
        let mut root=json!({"loreBookDepth":5,"temperature":1,"botPresetsId":"synthetic-preset-0",
            "selectedPersona":"synthetic-persona-0","personas":[
                {"id":"synthetic-persona-0","name":"Synthetic persona 0","personaPrompt":"Synthetic persona prompt 0","icon":"","note":"Synthetic persona note 0"},
                {"id":"synthetic-persona-1","name":"Synthetic persona 1","personaPrompt":"Synthetic persona prompt 1","icon":"","note":"Synthetic persona note 1"}]});
        let mut presets=vec![json!({"id":"synthetic-preset-0","name":"Synthetic preset 0"}),
            json!({"id":"synthetic-preset-1","name":"Synthetic preset 1"})];
        if let Some(endpoint)=&self.local_sse {
            let mirror=json!({"aiModel":"reverse_proxy","subModel":"reverse_proxy","customAPIFormat":0,
                "forceReplaceUrl":endpoint.url()?,"customProxyRequestModel":"synthetic-lww","proxyRequestModel":"custom",
                "proxyKey":"synthetic-only","reverseProxyOobaMode":false,"localNetworkMode":true,"localNetworkTimeoutSec":120,
                "maxContext":4096,"maxResponse":128,"temperature":1,"frequencyPenalty":0,"PresensePenalty":0,
                "currentPluginProvider":"","mainPrompt":"Synthetic local SSE fixture","jailbreak":"","globalNote":"",
                "formatingOrder":["main","description","personaPrompt","chats","lastChat","jailbreak","lorebook","globalNote","authorNote"],
                "modelTools":[],"seperateModelsForAxModels":false,
                "seperateModels":{"memory":"","emotion":"","translate":"","otherAx":""},
                "fallbackModels":{"memory":[],"emotion":[],"translate":[],"otherAx":[],"model":[]},
                "fallbackWhenBlankResponse":false});
            for (key,value) in mirror.as_object().unwrap() {
                root[key]=value.clone();for preset in &mut presets {preset[key]=value.clone();}
            }
            // These are Database flags rather than botPreset fields.
            for (key,value) in json!({"useStreaming":true,"customTokenizer":"tik","autofillRequestUrl":false,"genTime":1,
                "presetChain":"","promptTemplate":null,"personaNote":true,"username":"Synthetic persona 0",
                "personaPrompt":"Synthetic persona prompt 0","userNote":"Synthetic persona note 0","userIcon":"","autoTranslate":false,"translatorType":"none",
                "globalscript":[],"presetRegex":[],"plugins":[],"modules":[],"loadouts":[]}).as_object().unwrap() {
                root[key]=value.clone();
            }
            // An absent preset template leaves the actual nullable root template unchanged.
            for preset in &mut presets {preset["promptTemplate"]=Value::Null;}
        }
        if serde_json::to_vec(&root).map_err(|e|e.to_string())?.len()>1024*1024
            || presets.iter().any(|p|serde_json::to_vec(p).unwrap().len()>1024*1024) {
            return Err("fixture configuration exceeds bounded native metadata".into());
        }
        Ok((root,presets))
    }

    pub fn configure_character(&self,character:&mut Value) {
        if self.local_sse.is_none() {return;}
        for (key,value) in json!({"desc":"Synthetic local SSE character","firstMessage":"","customscript":[],
            "triggerscript":[],"supaMemory":false,"viewScreen":"none","modules":[]}).as_object().unwrap() {character[key]=value.clone();}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_and_provider_fields_are_local_bounded_and_coherent() {
        let configuration=NativeFixtureConfiguration {local_sse:Some(LocalSseEndpoint {address:"127.0.0.1".parse().unwrap(),port:32123})};
        let (root,presets)=configuration.initial_values().unwrap();
        for preset in &presets {
            for key in ["aiModel","subModel","forceReplaceUrl","customProxyRequestModel","localNetworkMode","modelTools"] {
                assert_eq!(preset[key],root[key]);
            }
            assert!(preset.get("useStreaming").is_none());
        }
        assert_eq!(root["forceReplaceUrl"],"http://127.0.0.1:32123/v1/chat/completions");
        assert_eq!(root["promptTemplate"],Value::Null);assert_eq!(root["presetChain"],"");
        assert!(root["plugins"].as_array().unwrap().is_empty());
        assert_eq!(LocalSseEndpoint {address:"10.0.2.2".parse().unwrap(),port:32123}.url().unwrap(),"http://10.0.2.2:32123/v1/chat/completions");
        for (address,port) in [("8.8.8.8",32123),("127.0.0.1",0),("0.0.0.0",32123)] {
            assert!(LocalSseEndpoint {address:address.parse().unwrap(),port}.url().is_err());
        }
    }
}

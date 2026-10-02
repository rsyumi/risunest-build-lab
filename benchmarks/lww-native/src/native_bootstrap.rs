use super::*;

#[derive(Serialize)]
pub(crate) struct RawServerBootstrap {
    pub status:&'static str,
    pub scenario:measurement::Scenario,
    pub direction:final_runner::Direction,
    pub iteration:u32,
    pub warmup:bool,
    pub policy:BootstrapPolicy,
    pub writer_before:String,
    pub writer_after:Option<String>,
    pub usable_database_ms:Option<f64>,
    pub all_bodies_local_ms:Option<f64>,
    pub activation_revision:Option<i64>,
    pub final_revision:Option<i64>,
    pub activation_request:Option<Value>,
    pub body_barrier_authority:Option<String>,
    pub native_observation:NativeObservation,
    pub source_end_and_shutdown:Option<Value>,
    pub asset_sha_and_publication_ledger:Option<Value>,
    pub missing_observers:Vec<&'static str>,
    pub error:Option<String>,
}
#[derive(Serialize)]
pub(crate) struct BootstrapPolicy {
    pub warmups:u32,
    pub recorded_repetitions:u32,
    pub percentile_resolution:&'static str,
    pub timing_scope:&'static str,
}

/// Candidate configuration and verified producer inventories are excluded setup.
/// The caller must supply a fresh unbound destination with its independent writer.
pub(crate) fn server_bootstrap_native_raw(destination:&mut PersistentStore,
    io:Arc<server_sync::client::TestIoCounters>,asset_hashes:&[String],selected_character_id:Option<&str>,
    direction:final_runner::Direction,iteration:u32)->Result<RawServerBootstrap,String> {
    if iteration>=6 {return Err("bootstrap slot exceeds frozen one-plus-five policy".into());}
    if destination.lww_binding_state().map_err(|e|e.to_string())?.target
        !=crate::persistent_store::sync_selection::SyncTarget::None {
        return Err("bootstrap destination is already bound".into());
    }
    let writer_before=destination.lww_clock_state().map_err(|e|e.to_string())?.writer_id;
    io.reset();reset_work(asset_hashes);
    let started=Instant::now();
    let mut usable_database_ms=None;
    let mut activation_revision=None;
    let mut body_barrier_authority=None;
    let mut all_bodies_local_ms=None;
    let mut completion=None;
    let outcome=(||->Result<(),String> {
        let bound=server_sync::first_binding_cycle(destination,io.clone(),|receipt| {
            usable_database_ms=Some(started.elapsed().as_secs_f64()*1000.0);
            activation_revision=Some(receipt.revision);
        }).map_err(|e|format!("{e:?}"))?;
        body_barrier_authority=Some(bound.state.target_authority.0.to_string());
        completion=Some(bound);
        server_sync::hydrate_binding_bodies(destination,io.clone(),selected_character_id,||{})
            .map_err(|e|format!("{e:?}"))?;
        all_bodies_local_ms=Some(started.elapsed().as_secs_f64()*1000.0);
        Ok(())
    })();
    let mut observation=take_work(false);
    let [requests,sent,received]=io.snapshot();observation.requests=requests;
    observation.uploaded_bytes=sent;observation.downloaded_bytes=received;
    let mut record=RawServerBootstrap {status:"INVALID",scenario:measurement::Scenario::FullBootstrap,
        direction,iteration,warmup:iteration==0,policy:BootstrapPolicy {warmups:1,recorded_repetitions:5,
            percentile_resolution:"N=5 nearest-rank p95 and p99 share the largest sample; population tail is unresolved",
            timing_scope:"native durable replacement callback and awaited Full body barrier; renderer adoption/paint unobserved"},
        writer_before,writer_after:None,usable_database_ms,all_bodies_local_ms,activation_revision,
        final_revision:None,activation_request:None,body_barrier_authority,native_observation:observation,
        source_end_and_shutdown:None,asset_sha_and_publication_ledger:None,
        missing_observers:vec!["verified producer source roles and complete source End plus final Shutdown"],error:outcome.err()};
    if let Err(reason)=record.native_observation.validate_costly_native_coverage() {
        record.missing_observers.push("complete caller and worker native SHA plus body publication ledger");
        record.error=Some(record.error.take().map_or(reason.clone(),|prior|format!("{prior}; {reason}")));
    } else {
        record.asset_sha_and_publication_ledger=Some(json!({
            "domains":record.native_observation.body_domains,"objects":record.native_observation.body_objects,
            "assetWork":record.native_observation.body_asset_work,"controlWork":record.native_observation.body_control_work,
            "nativeShaSubsetSemantics":"per-object body_sha is a subset of native caller/worker SHA totals, never added twice",
            "publicationByteSemantics":"successful link/rename identity size, not copied bytes",
        }));
    }
    let checks=verify_post_scope(&record.native_observation,||->Result<(),String> {
        let writer=destination.lww_clock_state().map_err(|e|e.to_string())?.writer_id;
        record.writer_after=Some(writer.clone());
        if writer!=record.writer_before {return Err("bootstrap changed destination writer".into());}
        let current=destination.revision().map_err(|e|e.to_string())?;
        record.final_revision=Some(current);
        if let Some(bound)=&completion {
            let request=&bound.activation_request;
            record.activation_request=Some(json!({
                "header":request.header,
                "expectedSelectionEpoch":request.expected_selection_epoch,
                "stagingId":request.staging_id,"receiveId":request.receive_id,
                "targetId":request.target_id,"libraryId":request.library_id,
            }));
            if record.activation_revision!=Some(bound.activation.revision) {return Err("actual activation callback receipt differs".into());}
            let replay=destination.replace_lww_binding(&bound.activation_request).map_err(|e|e.to_string())?;
            if replay.revision!=bound.activation.revision || destination.revision().map_err(|e|e.to_string())?!=current {
                return Err("durable activation receipt replay changed final state".into());
            }
        }
        if record.all_bodies_local_ms.is_some() {
            let cas=crate::asset_repository::PayloadCas::new(destination.repository_root()).map_err(|e|e.to_string())?;
            for hash in asset_hashes {
                if cas.stat_object(hash).map_err(|e|e.to_string())?.is_none() {return Err("awaited body barrier left required Asset absent".into());}
            }
        }
        Ok(())
    });
    if let Err(failed)=checks {
        record.error=Some(record.error.take().map_or(failed.reason.clone(),|prior|format!("{prior}; {}",failed.reason)));
    }
    Ok(record)
}

/// Source registration/configuration and publication happen before the measured scope.
pub(crate) fn source_client(source:&mut source_process::SourceProcess,store:&PersistentStore,
    name:&str,registration_request_id:&str,candidate:bool)->Result<LwwClient,String> {
    let registration=source.register(name,registration_request_id)?;
    let config=server_sync::client::ServerConfig::parse_uri(&registration.uri).map_err(|_|"source registration invalid")?;
    let client=server_sync::client::ServerClient::new(config.clone()).map_err(|e|format!("{e:?}"))?;
    client.resolve_identity(false).map_err(|e|format!("{e:?}"))?;
    let stored=server_sync::credentials::StoredConfig::persist(store.repository_root(),&config).map_err(|e|format!("{e:?}"))?;
    let mut core=LwwClient::new(store.repository_root(),config).map_err(|e|format!("{e:?}"))?;
    core.access=Some(stored.clone());core.client.test_io=Some(Arc::new(server_sync::client::TestIoCounters::default()));
    if candidate {
        server_sync::lww_client::OperationLog::open(store.repository_root()).map_err(|e|format!("{e:?}"))?
            .save_config("candidate",&stored).map_err(|e|format!("{e:?}"))?;
    }
    Ok(core)
}

pub(crate) fn server_bootstrap_source_raw(mut source:source_process::SourceProcess,
    destination:&mut PersistentStore,io:Arc<server_sync::client::TestIoCounters>,
    roles:&std::collections::BTreeMap<String,std::collections::BTreeSet<String>>,
    selected_character_id:Option<&str>,direction:final_runner::Direction,iteration:u32)
    ->Result<RawServerBootstrap,String> {
    let asset_hashes=roles.iter().filter(|(_,purposes)|purposes.contains("Asset"))
        .map(|(hash,_)|hash.clone()).collect::<Vec<_>>();
    source.begin(&format!("bootstrap-{direction:?}-{iteration}"),"native-full-bootstrap",roles)?;
    let outcome=server_bootstrap_native_raw(destination,io,&asset_hashes,selected_character_id,direction,iteration);
    // End and Shutdown are after actual transport/body completion and every native counter take.
    let settled=source.end().and_then(|_|source.shutdown());
    match outcome {
        Ok(mut record)=>{
            match settled {
                Ok(receipt)=>{
                    record.source_end_and_shutdown=Some(serde_json::to_value(receipt).map_err(|_|"source receipt encoding failed")?);
                    record.missing_observers.retain(|&missing|missing!="verified producer source roles and complete source End plus final Shutdown");
                },
                Err(reason)=>{
                    record.error=Some(record.error.take().map_or(reason.clone(),|prior|format!("{prior}; {reason}")));
                    record.source_end_and_shutdown=Some(json!({"status":"INVALID","raw":source.raw_observations}));
                },
            }
            // Native-only receipt still does not certify renderer adoption or producer inventory completeness.
            Ok(record)
        },
        Err(reason)=>Err(settled.err().map_or(reason.clone(),|cleanup|format!("{reason}; {cleanup}"))),
    }
}

#[derive(Serialize)]
pub(crate) struct RawAssetTransferOverlap {
    pub status:&'static str,pub direction:final_runner::Direction,pub iteration:u32,pub warmup:bool,
    pub source_reached:Option<Value>,pub source_release:Option<Value>,
    pub foreground_durable_ms:Option<f64>,pub foreground_publication_ms:Option<f64>,
    pub foreground_revision:Option<i64>,pub foreground_operation:Option<Value>,
    pub native_bodies_settled_ms:Option<f64>,pub worker_hashes:Option<Value>,
    pub native_observation:NativeObservation,pub source_end_and_shutdown:Option<Value>,
    pub errors:Vec<String>,
}

/// The destination is already activated; its body job uses the production independent connection.
pub(crate) fn server_asset_transfer_overlap_raw(mut source:source_process::SourceProcess,
    destination:&mut PersistentStore,sender:&LwwClient,peer:&mut PersistentStore,receiver:&LwwClient,
    roles:&std::collections::BTreeMap<String,std::collections::BTreeSet<String>>,barrier_hash:&str,
    selected_character_id:Option<&str>,direction:final_runner::Direction,iteration:u32)->Result<RawAssetTransferOverlap,String> {
    if iteration>=6 {return Err("asset-transfer slot exceeds one-plus-five policy".into());}
    let assets=roles.iter().filter(|(_,purposes)|purposes.contains("Asset")).map(|(hash,_)|hash.clone()).collect::<Vec<_>>();
    if !assets.iter().any(|hash|hash==barrier_hash) {return Err("physical barrier hash lacks verified Asset provenance".into());}
    let job_store=destination.open_native_job_store().map_err(|e|e.to_string())?;
    let selected=selected_character_id.map(str::to_owned);
    let io=Arc::new(server_sync::client::TestIoCounters::default());
    let foreground_io=sender.client.test_io.as_ref().ok_or("foreground IO observer absent")?;
    let peer_io=receiver.client.test_io.as_ref().ok_or("peer IO observer absent")?;
    foreground_io.reset();peer_io.reset();reset_work(&assets);
    for (hash,purposes) in roles {if purposes.contains("Control") {
        crate::asset_repository::body_io::register_object_purpose(hash,crate::asset_repository::body_io::BodyPurpose::Control);
    }}
    source.begin(&format!("asset-overlap-{iteration}"),"native-during-asset-transfer",roles)?;
    source.arm_read_barrier(&format!("asset-read-{iteration}"),barrier_hash)?;
    let token=crate::asset_repository::body_io::capture_body_io_scope();
    let worker_io=io.clone();let started=Instant::now();
    let worker=std::thread::spawn(move || {
        super::super::hash_work::reset_hash_work();
        server_sync::hash_metrics::reset_hash_metrics();
        let outcome=crate::asset_repository::body_io::with_body_io_scope(token,|| {
            server_sync::hydrate_binding_bodies(&job_store,worker_io,selected.as_deref(),||{})
                .map_err(|e|format!("{e:?}"))
        });
        (outcome,super::super::hash_work::take_hash_work(),server_sync::hash_metrics::take_hash_metrics(),
            format!("{:?}",std::thread::current().id()))
    });
    let mut errors=vec![];let mut reached=None;let mut released=None;
    let mut durable=None;let mut published=None;let mut revision=None;let mut receipt=None;
    let mut expected=None;let mut affected=None;
    match source.wait_read_barrier() {
        Ok(proof)=>{
            reached=Some(proof);
            let foreground=Instant::now();
            let operation=(||->Result<(),String> {
                let (commit,value)=prepare_edit(destination,SmallChange::Setting,final_direction(direction),iteration.into());
                expected=Some(value);
                let saved=destination.commit(&commit).map_err(|e|e.to_string())?;
                revision=Some(saved.revision);durable=Some(foreground.elapsed().as_secs_f64()*1000.0);
                receipt=server_sync::lww_tests::publish_cycle(sender,destination,&[]).map_err(|e|format!("{e:?}"))?;
                if receipt.is_none() {return Err("held-body foreground change was not published".into());}
                published=Some(foreground.elapsed().as_secs_f64()*1000.0);
                let applied=server_sync::lww_tests::receive_cycle(receiver,peer,&[]).map_err(|e|format!("{e:?}"))?;
                if applied.received_units==0 {return Err("held-body foreground receive was empty".into());}
                affected=Some(key_set(applied.result.affected_keys));
                Ok(())
            })();
            if let Err(reason)=operation {errors.push(reason);}
            match source.release_read_barrier() {Ok(proof)=>released=Some(proof),Err(reason)=>errors.push(reason)}
        },
        Err(reason)=>errors.push(reason),
    }
    let joined=worker.join();let body_ms=started.elapsed().as_secs_f64()*1000.0;
    let mut observation=take_work(false);let mut worker_hashes=None;let mut settled_ms=None;
    match joined {
        Ok((outcome,native,c,thread))=>{
            if let Err(reason)=outcome {errors.push(reason);}else {settled_ms=Some(body_ms);}
            let incomplete=native.incomplete.iter().map(|(domain,count)|format!("{domain}:{count}")).collect();
            let receipt=native_observation::NativeWorkerHashReceipt {thread,domains:native.domains.into_iter()
                .map(|(name,work)|(name.into(),native_observation::HashDomain {calls:work.calls,bytes:work.bytes})).collect(),incomplete};
            worker_hashes=Some(json!({"native":receipt,"C":c}));
            if let Err(reason)=observation.attach_worker_hash_receipt(receipt) {errors.push(reason);}
            for (domain,work) in c.domains {
                let total=observation.hashes.entry(format!("C/{domain}")).or_default();
                total.calls=total.calls.checked_add(work.calls).ok_or("worker C hash-call overflow")?;
                total.bytes=total.bytes.checked_add(work.bytes).ok_or("worker C hash-byte overflow")?;
            }
            if c.incomplete {observation.incomplete.push("C/body-worker SHA incomplete".into());}
        },
        Err(_)=>{errors.push("actual body worker panicked".into());observation.incomplete.push("unreturned native body-worker receipt".into());},
    }
    for counters in [&io,foreground_io,peer_io] {let [requests,sent,received]=counters.snapshot();
        observation.requests+=requests;observation.uploaded_bytes+=sent;observation.downloaded_bytes+=received;}
    let mut operation=None;
    if let Some(receipt)=receipt {
        let diagnostic=(||->Result<Value,String> {
            let (body,intent):(Vec<u8>,String)=sender.log.0.query_row("SELECT body,intent FROM publications WHERE id=?1",
                [&receipt.operation_id],|row|Ok((row.get(0)?,row.get(1)?))).map_err(|e|e.to_string())?;
            let request:risunest_sync_wire::lww::PushRequest=serde_json::from_slice(&body).map_err(|e|e.to_string())?;
            let publication:server_sync::lww_client::Publication=serde_json::from_str(&intent).map_err(|e|e.to_string())?;
            if publication.request!=request {return Err("foreground sealed request mismatch".into());}
            let selected=key_set(publication.entries.into_iter().map(|entry|entry.key));
            let submitted=key_set(request.changes.into_iter().map(|change|change.key));
            let winning=key_set(receipt.accepted_keys);
            if selected!=submitted || winning!=submitted || affected.as_ref()!=Some(&submitted) {
                return Err("actual foreground selected/submitted/winning/affected keys differ".into());
            }
            if let Some(expected)=&expected {
                if destination.read_root(None).map_err(|e|e.to_string())?.value["loreBookDepth"]!=*expected
                    || peer.read_root(None).map_err(|e|e.to_string())?.value["loreBookDepth"]!=*expected {
                    return Err("held-body foreground value did not settle on both native stores".into());
                }
            }else {return Err("foreground expected value is unobserved".into());}
            Ok(json!({"operationId":receipt.operation_id,"selectedKeys":selected,
                "submittedKeys":submitted,"winningKeys":winning,"affectedKeys":affected}))
        })();
        match diagnostic {Ok(value)=>operation=Some(value),Err(reason)=>errors.push(reason)}
    }
    if settled_ms.is_some() {
        let presence=(||->Result<(),String> {
            let cas=crate::asset_repository::PayloadCas::new(destination.repository_root()).map_err(|e|e.to_string())?;
            for hash in &assets {if cas.stat_object(hash).map_err(|e|e.to_string())?.is_none() {
                return Err("actual body completion left a declared Asset absent".into());
            }}
            Ok(())
        })();
        if let Err(reason)=presence {errors.push(reason);}
    }
    let settled=source.end().and_then(|_|source.shutdown());
    let source_receipt=match settled {Ok(receipt)=>Some(serde_json::to_value(receipt).map_err(|_|"source receipt encoding failed")?),
        Err(reason)=>{errors.push(reason);Some(json!({"status":"INVALID","raw":source.raw_observations}))}};
    if let Err(reason)=observation.validate_costly_native_coverage() {errors.push(reason);}
    Ok(RawAssetTransferOverlap {status:"INVALID",direction,iteration,warmup:iteration==0,source_reached:reached,source_release:released,
        foreground_durable_ms:durable,foreground_publication_ms:published,foreground_revision:revision,foreground_operation:operation,
        native_bodies_settled_ms:settled_ms,worker_hashes,native_observation:observation,source_end_and_shutdown:source_receipt,errors})
}

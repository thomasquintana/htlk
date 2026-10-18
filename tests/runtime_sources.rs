//! Source diagnostics are immutable protected sidecars, never execution authority.
mod support;
use htlk::{
    compiler::{
        JoinEdge, JoinEndpoint, JoinInput, JoinOperand, JoinOutput, JoinSpec, SourceLocation,
    },
    runtime::{
        Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
        failure::{FailureCode, RuntimeFailure},
        records::SecurityLabels,
        source::{SourceAttachment, SourceEntry, SourceOrigin, SourceSidecar},
        store::{SqliteStore, StoreConfig},
    },
};
use htlk_executable::{
    Identifier, JsonPointer,
    cbor::{Limits, Value},
    digest::{Digest, hash_bytes},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
struct Policy {
    private: AtomicBool,
}
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if matches!(action, Authorization::AttachSource { .. }) && context.principal() != "editor" {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        if let Authorization::ReadSource { labels, .. } = action
            && context.principal() == "viewer"
            && !labels.as_slice().is_empty()
        {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        Ok(())
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        _: Option<&Value>,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::default())
    }
    fn source_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &SourceSidecar,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(if self.private.load(Ordering::SeqCst) {
            SecurityLabels::new(vec!["source-private".into()], &Limits::default()).unwrap()
        } else {
            SecurityLabels::default()
        })
    }
}
async fn sources(
    runtime: &Runtime,
    context: &HostContext,
    fingerprint: Digest,
) -> Vec<SourceAttachment> {
    let mut after = None;
    let mut all = Vec::new();
    loop {
        let page = runtime
            .inspect_sources(context, fingerprint, after, 1)
            .await
            .unwrap();
        all.extend(page.sources);
        if page.next.is_none() {
            return all;
        }
        after = page.next;
    }
}
#[test]
fn source_sidecars_preserve_identity_multiple_origins_join_paths_and_inherited_protection() {
    actix::System::new().block_on(async {
        const SOURCE:&str=r#"ir_version="0.1" graph test.source{inputs={value=string}outputs={value=string}nodes{}edges{edge value{from=inputs.value to=outputs.value}}}"#;
        let (compiler,registry)=support::native_compiler();let catalogs=support::catalogs();let old=compiler.compile_source(SOURCE,&catalogs).unwrap();let added=compiler.compile_source(r#"ir_version="0.1" graph test.added_source{inputs={value=string}outputs={value=string}nodes{copy=eval(string,inputs.value){inputs={value=string}}}edges{edge input{from=inputs.value to=copy.inputs.value}edge output{from=copy.outputs.value to=outputs.value}}}"#,&catalogs).unwrap();
        let endpoint=|graph,port:&str|JoinEndpoint{graph,port:port.into()};let composed=compiler.join_graphs(&old.canonical_document,&added.canonical_document,&JoinSpec{id:"test.source_joined".into(),left_alias:"preserved".into(),right_alias:"added".into(),edges:vec![JoinEdge{id:"value".into(),from:endpoint(JoinOperand::Left,"value"),to:endpoint(JoinOperand::Right,"value")}],inputs:vec![JoinInput{name:"value".into(),to:vec![endpoint(JoinOperand::Left,"value")]}],outputs:vec![JoinOutput{name:"value".into(),from:endpoint(JoinOperand::Right,"value")}]},&catalogs).unwrap();
        let policy=Arc::new(Policy{private:AtomicBool::new(true)});let store=SqliteStore::in_memory(StoreConfig::default()).await.unwrap();let runtime=Runtime::new(store.clone(),Arc::new(registry),policy.clone(),RuntimeConfig::default()).unwrap();let editor=HostContext::new("editor".into(),&Limits::default()).unwrap();let viewer=HostContext::new("viewer".into(),&Limits::default()).unwrap();for graph in [&old,&composed]{runtime.register_graph(&editor,&graph.executable_bytes,&[]).await.unwrap();}
        let entries=old.source_map.iter().map(|e|SourceEntry{canonical_path:JsonPointer::new(&e.canonical_path,&Limits::default()).unwrap(),origin:match &e.span.source{SourceLocation::Inline=>SourceOrigin::Inline,SourceLocation::Module{package_digest,module_path,source_path}=>SourceOrigin::Module{package_digest:*package_digest,module_path:module_path.clone(),source_path:source_path.clone()}},span:Some((e.span.start_byte as u64,e.span.end_byte as u64)),declaration_name:e.declaration_name.clone(),description:Some("Display-only original description.".into())}).collect();let mut sidecar=SourceSidecar{provenance:hash_bytes(SOURCE.as_bytes()),entries};assert!(!sidecar.entries.is_empty());let id=runtime.attach_source(&editor,old.fingerprint,&sidecar).await.unwrap();assert_eq!(runtime.attach_source(&editor,old.fingerprint,&sidecar).await.unwrap(),id);assert_eq!(sources(&runtime,&editor,old.fingerprint).await.len(),1);assert!(sources(&runtime,&viewer,old.fingerprint).await.is_empty());assert_eq!(runtime.attach_source(&viewer,old.fingerprint,&sidecar).await.unwrap_err().code(),FailureCode::Policy);
        sidecar.entries[0].description=Some("A second source interpretation.".into());let other=runtime.attach_source(&editor,old.fingerprint,&sidecar).await.unwrap();assert_ne!(id,other);let mut invalid=sidecar.clone();invalid.entries[0].canonical_path=JsonPointer::new("/not_a_record",&Limits::default()).unwrap();assert!(runtime.attach_source(&editor,old.fingerprint,&invalid).await.is_err());assert_eq!(sources(&runtime,&editor,old.fingerprint).await.len(),2);
        assert_eq!(runtime.inspect_graph(&viewer,old.fingerprint).await.unwrap().document,old.canonical_document,"source changes cannot rewrite canonical code");policy.private.store(false,Ordering::SeqCst);assert!(!runtime.inherit_sources(&editor,old.fingerprint,composed.fingerprint).await.unwrap().is_empty());let inherited=sources(&runtime,&editor,composed.fingerprint).await;assert!(!inherited.is_empty());assert!(inherited.iter().all(|s|s.labels.as_slice()==["source-private".to_string()]));assert!(sources(&runtime,&viewer,composed.fingerprint).await.is_empty());
        let generated=SourceSidecar{provenance:hash_bytes(b"binary join report"),entries:vec![SourceEntry{canonical_path:JsonPointer::new(&format!("/scopes/{}",composed.canonical_document.fields().root_scope),&Limits::default()).unwrap(),origin:SourceOrigin::Synthetic{reason:"binary join wrapper".into()},span:None,declaration_name:None,description:Some("Generated composition boundary.".into())}]};runtime.attach_source(&editor,composed.fingerprint,&generated).await.unwrap();let visible=sources(&runtime,&viewer,composed.fingerprint).await;assert_eq!(visible.len(),1);assert!(matches!(visible[0].sidecar.entries[0].origin,SourceOrigin::Synthetic{..}));assert_eq!(runtime.inspect_graph(&editor,composed.fingerprint).await.unwrap().document,composed.canonical_document);
        let historical=Runtime::new(store,Arc::new(htlk::runtime::NativeRegistry::new(&Limits::default()).unwrap()),policy,RuntimeConfig::default()).unwrap();let view=historical.inspect_graph(&editor,old.fingerprint).await.unwrap();assert!(!view.native_available);assert_eq!(view.document,old.canonical_document);assert_eq!(sources(&historical,&editor,old.fingerprint).await.len(),2);
    })
}

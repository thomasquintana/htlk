//! Explicit Serde serialization through canonical model wire views.
//!
//! Record serialization uses default codec limits. Callers with different limits
//! prepare a bounded `to_value` view and serialize that view instead. Generic
//! Serde output is not a replacement for canonical CBOR/JSON ingress validation.
use crate::*;
use serde::{Serialize, Serializer, ser::Error as _};

macro_rules! record {
    ($($ty:ty),* $(,)?)=>{$(
        impl Serialize for $ty {
            fn serialize<S:Serializer>(&self,serializer:S)->Result<S::Ok,S::Error> {
                self.to_value(&cbor::Limits::default()).map_err(S::Error::custom)?.serialize(serializer)
            }
        }
    )*};
}
record!(
    CanonicalDocument,
    EngineIdentity,
    ExecutionProfile,
    FunctionSignature,
    Library,
    ServerIdentity,
    McpBinding,
    ExecutionLimits,
    RetryPolicy,
    PortTable,
    Operation,
    PromptTemplate
);

macro_rules! contextual_record {
    ($context:expr;$($ty:ty),* $(,)?)=>{$(
        impl Serialize for $ty {
            fn serialize<S:Serializer>(&self,serializer:S)->Result<S::Ok,S::Error> {
                self.to_value($context,&cbor::Limits::default()).map_err(S::Error::custom)?.serialize(serializer)
            }
        }
    )*};
}
contextual_record!(ScopeContext::Ordinary;Scope,Node,Edge);
contextual_record!(ExpressionContext::Eval;Expression);
contextual_record!(TypeContext::Signature;ValueType,Port);

impl Serialize for Identifier {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl Serialize for digest::Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
impl Serialize for JsonPointer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
impl Serialize for JsonDocument {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value().serialize(serializer)
    }
}
impl Serialize for PolicyDocument {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.document().serialize(serializer)
    }
}

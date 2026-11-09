//! Packs: optional, domain-specific sets of components, data formats and languages (the health
//! pack: HL7 v2, MLLP, FHIR, DICOM), compiled in when a build asks for them and added to a
//! context with one call.
//!
//! A pack describes itself with a static [`PackInfo`]. [`CamelContext::add_pack`] registers it and
//! records the description, so `cameleer version`, validation, the YAML DSL (pack languages are
//! expression keys) and missing-component messages know about it without central tables.

use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::context::CamelContext;

/// Static description of a pack.
#[derive(Debug, PartialEq, Eq)]
pub struct PackInfo {
    /// Short name (`health`).
    pub name: &'static str,
    /// The pack crate's version.
    pub version: &'static str,
    /// The `cameleer-core` version the pack was built against.
    pub core_api: &'static str,
    pub description: &'static str,
    /// Component schemes it adds.
    pub components: &'static [&'static str],
    /// Data format names it adds.
    pub data_formats: &'static [&'static str],
    /// Expression languages it adds (also accepted as YAML and XML DSL expression keys).
    pub languages: &'static [&'static str],
    /// Simple function prefixes it adds (`dicom` for `${dicom.PatientID}`).
    pub simple_prefixes: &'static [&'static str],
}

impl PackInfo {
    /// `health 0.6.0 (core 0.6.0)`.
    pub fn display(&self) -> String {
        format!("{} {} (core {})", self.name, self.version, self.core_api)
    }
}

/// A pack.
pub trait Pack: Send + Sync {
    fn info(&self) -> &'static PackInfo;
    /// Register the pack's components, data formats, languages and Simple functions.
    fn register(&self, ctx: &CamelContext);
}

const PACKS_BEAN: &str = "cameleer.packs";

struct Installed(Mutex<Vec<&'static PackInfo>>);

fn known() -> &'static RwLock<Vec<&'static PackInfo>> {
    static K: OnceLock<RwLock<Vec<&'static PackInfo>>> = OnceLock::new();
    K.get_or_init(|| RwLock::new(Vec::new()))
}

/// Every pack added to any context in this process (the YAML DSL and validation, which may run
/// without a context, use this).
pub fn known_packs() -> Vec<&'static PackInfo> {
    known().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Whether a pack added in this process provides the expression language `name`.
pub fn is_pack_language(name: &str) -> bool {
    known().read().unwrap_or_else(|e| e.into_inner()).iter().any(|p| p.languages.contains(&name))
}

/// Make a pack known to the process without a context (for tools that parse routes first).
pub fn announce(info: &'static PackInfo) {
    let mut k = known().write().unwrap_or_else(|e| e.into_inner());
    if !k.iter().any(|p| p.name == info.name) {
        k.push(info);
    }
}

impl CamelContext {
    /// Register a pack on this context and record its description. Adding the same pack twice
    /// registers it once.
    pub fn add_pack(&self, pack: &dyn Pack) {
        let info = pack.info();
        let installed = match self.registry().lookup::<Installed>(PACKS_BEAN) {
            Some(i) => i,
            None => {
                let i = Arc::new(Installed(Mutex::new(Vec::new())));
                self.registry().bind_arc(PACKS_BEAN, i.clone());
                i
            }
        };
        {
            let mut g = installed.0.lock().unwrap_or_else(|e| e.into_inner());
            if g.iter().any(|p| p.name == info.name) {
                return;
            }
            g.push(info);
        }
        announce(info);
        pack.register(self);
    }

    /// The packs added to this context, in order.
    pub fn packs(&self) -> Vec<&'static PackInfo> {
        self.registry().lookup::<Installed>(PACKS_BEAN).map(|i| i.0.lock().unwrap_or_else(|e| e.into_inner()).clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static INFO: PackInfo = PackInfo {
        name: "testpack",
        version: "1.2.3",
        core_api: env!("CARGO_PKG_VERSION"),
        description: "a test pack",
        components: &["tp"],
        data_formats: &[],
        languages: &["tplang"],
        simple_prefixes: &[],
    };

    struct TestPack(std::sync::atomic::AtomicUsize);

    impl Pack for TestPack {
        fn info(&self) -> &'static PackInfo {
            &INFO
        }
        fn register(&self, _ctx: &CamelContext) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn packs_register_once_and_are_listed() {
        let p = TestPack(Default::default());
        let ctx = CamelContext::new();
        assert!(ctx.packs().is_empty());
        ctx.add_pack(&p);
        ctx.add_pack(&p);
        assert_eq!(p.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(ctx.packs(), vec![&INFO]);
        assert!(is_pack_language("tplang") && !is_pack_language("nope"));
        assert_eq!(INFO.display(), format!("testpack 1.2.3 (core {})", env!("CARGO_PKG_VERSION")));
        assert!(known_packs().contains(&&INFO));
    }
}

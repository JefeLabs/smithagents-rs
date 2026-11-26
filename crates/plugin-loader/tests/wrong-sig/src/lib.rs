//! Exports the plugin entry's symbol with a signature other than
//! `DylibEntry`: the loader must refuse it from stabby's type report alone,
//! without calling it.

#[stabby::export]
pub extern "C" fn smithagents_plugin_create_v1(seed: u32) -> u32 {
    seed
}

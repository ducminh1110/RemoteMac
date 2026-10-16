//! (placeholder until the demo Mac lands)
pub struct DemoMac {
    pub relay: String,
    pub session: String,
    pub token: String,
}

pub fn start() -> Result<DemoMac, String> {
    Err("the demo Mac is not in this build".into())
}

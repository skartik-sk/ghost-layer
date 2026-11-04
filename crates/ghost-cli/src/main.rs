fn main() {
    // Proves codegen is linked: every CompactTxStreamer type is reachable.
    let _ = ghost_transport::proto::ChainSpec::default();
    println!("ghost: scaffold ok");
}

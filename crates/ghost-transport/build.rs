fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(
            &["../../proto/service.proto", "../../proto/compact_formats.proto"],
            &["../../proto"],
        )?;
    Ok(())
}

fn main() {
    slint_build::compile_with_config(
        "../ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .with_style("fluent".into())
            .with_include_paths(vec![std::path::PathBuf::from("../ui")]),
    )
    .expect("compile Slint UI");
}

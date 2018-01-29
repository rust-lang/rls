use super::*;

#[test]
fn warn_missing_root_cargo_toml() {
    let mut env = Environment::new("no_cargo_toml");

    let root_path = env.cache.abs_path(Path::new("."));
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(
        ls_server::LsService::handle_message(&mut server),
        ls_server::ServerStateChange::Continue
    );
    {
        wait_for_n_results!(1, results);
        let response = json::parse(&results.lock().unwrap().remove(0)).unwrap();
        println!("{}", response.pretty(2));
        assert!(response["result"]["capabilities"].is_object());
    }
    {
        // showMessage warning
        wait_for_n_results!(1, results);
        let response = json::parse(&results.lock().unwrap().remove(0)).unwrap();
        println!("{}", response.pretty(2));
        assert_eq!(response["method"], "window/showMessage");
        assert_eq!(response["params"]["type"], 2, "Expected 'warning' message type");
        let message = response["params"]["message"].as_str().unwrap();
        assert!(message.contains("Cargo.toml"));
    }
}

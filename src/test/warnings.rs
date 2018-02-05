use super::*;

/// Rls should send a warning showMessage when we cannot find the project Cargo.toml
#[test]
fn warn_if_missing_cargo_toml() {
    // use a temp dir to avoid rls' own Cargo.toml affecting test project
    let dir = test_data_to_tmp_dir("no_cargo_toml").unwrap();

    let mut env = Environment::new(dir.path());

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

    // normally get 4 results back so expect a warning in the first 5 messages
    let mut received_warning = false;
    for m in 0..5 {
        if m == 4 {
            wait_for_n_results!(1, results, timeout = Duration::from_secs(5))
        } else {
            wait_for_n_results!(1, results)
        };
        let response = json::parse(&results.lock().unwrap().remove(0)).unwrap();
        println!("{}", response.pretty(2));
        if response["method"] == "window/showMessage" {
            assert_eq!(response["params"]["type"], 2, "Expected 'warning' message type");
            let message = response["params"]["message"].as_str().unwrap();
            assert!(message.contains("Cargo.toml"));
            assert!(message.to_lowercase().contains("could not find"));
            received_warning = true;
            break;
        }
    }

    assert!(received_warning);
}

/// Rls should send a warning showMessage when we find but cannot parse the project Cargo.toml
#[test]
fn warn_if_invalid_cargo_toml() {
    let mut env = Environment::new("dodgy_cargo_toml");

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

    // normally get 4 results back so expect a warning in the first 5 messages
    let mut received_warning = false;
    for m in 0..5 {
        if m == 4 {
            wait_for_n_results!(1, results, timeout = Duration::from_secs(5))
        } else {
            wait_for_n_results!(1, results)
        };
        let response = json::parse(&results.lock().unwrap().remove(0)).unwrap();
        println!("{}", response.pretty(2));
        if response["method"] == "window/showMessage" {
            assert_eq!(response["params"]["type"], 2, "Expected 'warning' message type");
            let message = response["params"]["message"].as_str().unwrap();
            assert!(message.contains("Cargo.toml"));
            assert!(message.to_lowercase().contains("failed to parse"));
            received_warning = true;
            break;
        }
    }

    assert!(received_warning);
}

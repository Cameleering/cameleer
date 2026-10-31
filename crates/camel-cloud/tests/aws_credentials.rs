//! Container and instance-metadata credentials of the AWS default chain, against the
//! IMDS/container emulator and the SigV4-verifying AWS emulator. This binary changes process
//! environment variables, so everything runs in one sequential test.

use camel_cloud::aws::{credentials, resolve_credentials, AwsConfig};
use camel_cloud::testing::{AwsMock, ImdsMock};
use camel_core::prelude::*;

#[test]
fn container_and_instance_profile_credentials() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("q");
    aws.add_session_credentials("ASIAINSTANCE", "instance-secret", "instance-token");
    let imds = ImdsMock::start("app-role", "ASIAINSTANCE", "instance-secret", "instance-token").unwrap();

    // pure resolution with an injected environment: IMDSv2
    let cfg = AwsConfig { region: "us-east-1".into(), use_default_credentials_provider: true, ..Default::default() };
    let url = imds.url();
    let env_imds = |k: &str| match k {
        "AWS_EC2_METADATA_SERVICE_ENDPOINT" => Some(url.clone()),
        "AWS_SHARED_CREDENTIALS_FILE" => Some("/nonexistent/credentials".to_string()),
        _ => None,
    };
    let c = resolve_credentials(&cfg, &env_imds).unwrap();
    assert_eq!((c.access_key.as_str(), c.session_token.as_deref()), ("ASIAINSTANCE", Some("instance-token")));
    assert_eq!(imds.credential_requests(), 1);
    // cached until close to expiry
    resolve_credentials(&cfg, &env_imds).unwrap();
    assert_eq!(imds.credential_requests(), 1);

    // container endpoint: a wrong authorization token is rejected (falls through to IMDS, disabled here)
    credentials::clear_cache();
    let uri = imds.container_uri();
    let env_bad = |k: &str| match k {
        "AWS_CONTAINER_CREDENTIALS_FULL_URI" => Some(uri.clone()),
        "AWS_CONTAINER_AUTHORIZATION_TOKEN" => Some("wrong".to_string()),
        "AWS_EC2_METADATA_DISABLED" => Some("true".to_string()),
        "AWS_SHARED_CREDENTIALS_FILE" => Some("/nonexistent/credentials".to_string()),
        _ => None,
    };
    let e = resolve_credentials(&cfg, &env_bad).unwrap_err();
    assert!(e.is_a("SdkClientException"), "{}", e);
    assert!(imds.rejected() >= 1);

    // end to end through a component with the real process environment
    credentials::clear_cache();
    for k in ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "AWS_PROFILE", "AWS_EC2_METADATA_DISABLED", "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI"] {
        std::env::remove_var(k);
    }
    std::env::set_var("AWS_SHARED_CREDENTIALS_FILE", "/nonexistent/credentials");
    std::env::set_var("AWS_CONTAINER_CREDENTIALS_FULL_URI", imds.container_uri());
    std::env::set_var("AWS_CONTAINER_AUTHORIZATION_TOKEN", imds.container_token());
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    let t = ctx.create_producer_template();
    let sqs = format!("aws2-sqs://q?useDefaultCredentialsProvider=true&region=us-east-1&uriEndpointOverride={}&overrideEndpoint=true", aws.url());
    t.request_body(&sqs, "via container credentials").unwrap();
    // instance metadata (IMDSv2) when no container endpoint is configured
    std::env::remove_var("AWS_CONTAINER_CREDENTIALS_FULL_URI");
    std::env::remove_var("AWS_CONTAINER_AUTHORIZATION_TOKEN");
    std::env::set_var("AWS_EC2_METADATA_SERVICE_ENDPOINT", imds.url());
    t.request_body(&sqs, "via instance profile").unwrap();
    assert_eq!(aws.queue_messages("q"), vec!["via container credentials", "via instance profile"]);
    assert_eq!(aws.auth_failures(), 0);
    // an IMDSv1-only endpoint (no session tokens) is supported as a fallback
    credentials::clear_cache();
    imds.set_v1_only(true);
    let before = imds.credential_requests();
    assert_eq!(resolve_credentials(&cfg, &env_imds).unwrap().access_key, "ASIAINSTANCE");
    assert_eq!(imds.credential_requests(), before + 1);
    std::env::remove_var("AWS_EC2_METADATA_SERVICE_ENDPOINT");
    std::env::remove_var("AWS_SHARED_CREDENTIALS_FILE");
}

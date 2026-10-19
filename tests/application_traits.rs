#[test]
fn application_can_be_transferred_between_threads() {
    fn assert_send<T: Send>() {}
    assert_send::<wayapp::Application>();
}

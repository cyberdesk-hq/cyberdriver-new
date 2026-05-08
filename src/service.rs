use librustdesk::*;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    #[cfg(feature = "cyberdesk")]
    crate::cyberdesk_branding::init();
    crate::common::load_custom_client();
    hbb_common::init_log(false, "service");
    crate::start_os_service();
}

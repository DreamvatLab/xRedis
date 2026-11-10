//! xredis-app:视图模型层(App、Msg、Cmd、Runtime)。不依赖任何 GUI。

pub fn core_name() -> &'static str {
    xredis_core::crate_name()
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_sees_core() {
        assert_eq!(super::core_name(), "xredis-core");
    }
}

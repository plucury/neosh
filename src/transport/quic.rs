#[derive(Debug, Clone)]
pub struct QuicSkeletonConfig {
    pub alpn: &'static str,
    pub max_control_frame_bytes: usize,
}

impl Default for QuicSkeletonConfig {
    fn default() -> Self {
        Self {
            alpn: "neosh/1",
            max_control_frame_bytes: 64 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_quic_skeleton_matches_protocol() {
        let cfg = QuicSkeletonConfig::default();
        assert_eq!(cfg.alpn, "neosh/1");
        assert!(cfg.max_control_frame_bytes >= 4096);
    }
}

//! 请求边界。
//!
//! 管道协议本身没有长度概念：一行就是一帧。客户端出一次 bug（比如把整张
//! 4K 截图编码两遍）就能让服务端把几十 MB 读进内存，而 `read_line` 会一直
//! 长到换行符出现为止。这一层把"最多能有多大"写成明确的上限，超了就在
//! 读的层面截断，而不是等 OOM。
//!
//! 全部可以用环境变量覆盖，方便在没有界面的机器上按机器规格调整：
//!
//! | 变量 | 默认 | 含义 |
//! |---|---|---|
//! | `MH_MAX_MESSAGE_MB`   | 32  | 单行请求最大字节 |
//! | `MH_MAX_IMAGES`       | 8   | 单次请求最多几张截图 |
//! | `MH_MAX_IMAGE_MP`     | 40  | 单张截图最大像素（百万） |
//! | `MH_MAX_CONNECTIONS`  | 8   | 同时服务的管道连接数 |
//! | `MH_REQUEST_TIMEOUT_SEC` | 180 | 单次识别的最长执行时间 |
//!
//! 关于"空闲超时"：客户端持的是长连接（`NamedPipeClient` 一个连接用到进程
//! 结束），服务端主动踢掉空闲连接只会让它每次都重连，收益为零。所以这里
//! 不做空闲超时——该由调用方（客户端 30 秒调用超时）负责；服务端只保证
//! **不会无限期地卡在一次请求里**，见 `max_request_duration`。

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Limits {
    /// 单行请求的最大字节数。
    pub max_message_bytes: usize,
    /// 单次请求最多几张截图。
    pub max_images: usize,
    /// 单张图最大像素数。
    pub max_image_pixels: u64,
    /// 单张图 base64 解码后的最大字节数。
    pub max_image_bytes: u64,
    /// 同时服务的连接数上限。
    pub max_connections: usize,
    /// 单次识别的最长执行时间（协作式检查，不是硬中断）。
    pub max_request_duration: Duration,
    /// 超过这个时长就打一条慢请求日志。
    pub slow_request: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: 32 * 1024 * 1024,
            max_images: 8,
            max_image_pixels: 40_000_000,
            max_image_bytes: 48 * 1024 * 1024,
            max_connections: 8,
            max_request_duration: Duration::from_secs(180),
            slow_request: Duration::from_secs(5),
        }
    }
}

impl Limits {
    /// 从环境变量取值，非法值退回默认并记一条警告——启动不能因为一个
    /// 打错的变量名就失败。
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            max_message_bytes: env_mb("MH_MAX_MESSAGE_MB", d.max_message_bytes),
            max_images: env_usize("MH_MAX_IMAGES", d.max_images).max(1),
            max_image_pixels: env_usize("MH_MAX_IMAGE_MP", 40) as u64 * 1_000_000,
            max_image_bytes: env_mb("MH_MAX_IMAGE_MB", d.max_image_bytes as usize) as u64,
            max_connections: env_usize("MH_MAX_CONNECTIONS", d.max_connections).max(1),
            max_request_duration: Duration::from_secs(
                env_usize("MH_REQUEST_TIMEOUT_SEC", d.max_request_duration.as_secs() as usize).max(1)
                    as u64,
            ),
            slow_request: d.slow_request,
        }
    }

    /// 人类可读的一行，启动时写进日志，出问题时能立刻看到当时的上限。
    pub fn describe(&self) -> String {
        format!(
            "limits: message<={}MB images<={} image<={}MP/{}MB connections<={} timeout={}s",
            self.max_message_bytes / (1024 * 1024),
            self.max_images,
            self.max_image_pixels / 1_000_000,
            self.max_image_bytes / (1024 * 1024),
            self.max_connections,
            self.max_request_duration.as_secs(),
        )
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => {
                tracing::warn!("{key}='{raw}' 不是正整数，使用默认值 {default}");
                default
            }
        },
        Err(_) => default,
    }
}

fn env_mb(key: &str, default_bytes: usize) -> usize {
    env_usize(key, default_bytes / (1024 * 1024)) * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded_and_sane() {
        let l = Limits::default();
        assert!(l.max_message_bytes >= 8 * 1024 * 1024, "4K 截图 base64 后约 3-6 MB，太小会误伤");
        assert!(l.max_images >= 2, "人物属性 + 包裹是常见的两张一起传");
        assert!(l.max_image_pixels >= 8_000_000, "4K 截图约 8.3 MP");
        assert!(l.slow_request < l.max_request_duration);
        assert!(l.describe().contains("message<=32MB"), "{}", l.describe());
    }
}

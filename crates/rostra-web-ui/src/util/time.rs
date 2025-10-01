use rostra_core::Timestamp;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn format_timestamp(timestamp: Timestamp) -> String {
    let system_time: SystemTime = UNIX_EPOCH + std::time::Duration::from_secs(timestamp.0);
    let now = SystemTime::now();
    let duration_since = now.duration_since(system_time).unwrap_or_default();
    
    let seconds = duration_since.as_secs();
    
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else if seconds < 2592000 { // 30 days
        format!("{}d", seconds / 86400)
    } else {
        // For older posts, show the actual date
        let timestamp_secs = system_time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        // Simple date formatting (would be better with chrono crate)
        let days_since_epoch = timestamp_secs / 86400;
        let year = 1970 + days_since_epoch / 365;
        let day_of_year = days_since_epoch % 365;
        let month = day_of_year / 30 + 1;
        let day = day_of_year % 30 + 1;
        
        format!("{}/{}/{}", month, day, year)
    }
}
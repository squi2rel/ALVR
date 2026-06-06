use alvr_common::SlidingWindowAverage;
use alvr_packets::ClientStatistics;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_VALID_VSYNC_QUEUE: Duration = Duration::from_secs(1);
const MAX_VALID_TOTAL_PIPELINE_LATENCY: Duration = Duration::from_secs(1);

struct HistoryFrame {
    input_acquired: Instant,
    video_packet_received: Instant,
    client_stats: ClientStatistics,
}

pub struct StatisticsManager {
    history_buffer: VecDeque<HistoryFrame>,
    max_history_size: usize,
    prev_vsync: Instant,
    total_pipeline_latency_average: SlidingWindowAverage<Duration>,
}

impl StatisticsManager {
    pub fn new(max_history_size: usize) -> Self {
        Self {
            max_history_size,
            history_buffer: VecDeque::new(),
            prev_vsync: Instant::now(),
            total_pipeline_latency_average: SlidingWindowAverage::new(
                Duration::ZERO,
                max_history_size,
            ),
        }
    }

    pub fn report_input_acquired(&mut self, target_timestamp: Duration) {
        if !self
            .history_buffer
            .iter()
            .any(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            self.history_buffer.push_front(HistoryFrame {
                input_acquired: Instant::now(),
                // this is just a placeholder because Instant does not have a default value
                video_packet_received: Instant::now(),
                client_stats: ClientStatistics {
                    target_timestamp,
                    ..Default::default()
                },
            });
        }

        if self.history_buffer.len() > self.max_history_size {
            self.history_buffer.pop_back();
        }
    }

    pub fn report_video_packet_received(&mut self, target_timestamp: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            frame.video_packet_received = Instant::now();
        }
    }

    pub fn report_frame_decoded(&mut self, target_timestamp: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            frame.client_stats.video_decode =
                Instant::now().saturating_duration_since(frame.video_packet_received);
        }
    }

    pub fn report_compositor_start(&mut self, target_timestamp: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            frame.client_stats.video_decoder_queue = Instant::now().saturating_duration_since(
                frame.video_packet_received + frame.client_stats.video_decode,
            );
        }
    }

    // vsync_queue is the latency between this call and the vsync. it cannot be measured by ALVR and
    // should be reported by the VR runtime
    pub fn report_submit(
        &mut self,
        target_timestamp: Duration,
        vsync_queue: Duration,
    ) -> Option<ClientStatistics> {
        let now = Instant::now();

        let frame_index = self
            .history_buffer
            .iter()
            .position(|frame| frame.client_stats.target_timestamp == target_timestamp)?;

        let frame = &self.history_buffer[frame_index];
        let rendering = now.saturating_duration_since(
            frame.video_packet_received
                + frame.client_stats.video_decode
                + frame.client_stats.video_decoder_queue,
        );
        let pipeline_latency_before_vsync = now.saturating_duration_since(frame.input_acquired);
        let total_pipeline_latency = pipeline_latency_before_vsync.checked_add(vsync_queue);

        let valid_sample = vsync_queue <= MAX_VALID_VSYNC_QUEUE
            && pipeline_latency_before_vsync <= MAX_VALID_TOTAL_PIPELINE_LATENCY
            && total_pipeline_latency
                .is_some_and(|latency| latency <= MAX_VALID_TOTAL_PIPELINE_LATENCY);

        if !valid_sample {
            self.history_buffer.remove(frame_index);

            return None;
        }

        let total_pipeline_latency = total_pipeline_latency.unwrap();

        let frame = &mut self.history_buffer[frame_index];
        frame.client_stats.rendering = rendering;
        frame.client_stats.vsync_queue = vsync_queue;
        frame.client_stats.total_pipeline_latency = total_pipeline_latency;
        self.total_pipeline_latency_average
            .submit_sample(frame.client_stats.total_pipeline_latency);

        let vsync = now + vsync_queue;
        frame.client_stats.frame_interval = vsync.saturating_duration_since(self.prev_vsync);
        self.prev_vsync = vsync;

        Some(frame.client_stats.clone())
    }

    // latency used for head prediction
    pub fn average_total_pipeline_latency(&self) -> Duration {
        self.total_pipeline_latency_average.get_average()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_unreasonable_vsync_queue_samples() {
        let mut stats = StatisticsManager::new(16);
        let timestamp = Duration::from_secs(1);

        stats.report_input_acquired(timestamp);

        assert!(
            stats
                .report_submit(timestamp, Duration::from_millis(3_270_543))
                .is_none()
        );
        assert_eq!(stats.average_total_pipeline_latency(), Duration::ZERO);
    }

    #[test]
    fn keeps_reasonable_submit_samples() {
        let mut stats = StatisticsManager::new(16);
        let timestamp = Duration::from_secs(1);

        stats.report_input_acquired(timestamp);

        let client_stats = stats
            .report_submit(timestamp, Duration::from_millis(5))
            .unwrap();

        assert_eq!(client_stats.target_timestamp, timestamp);
        assert_eq!(client_stats.vsync_queue, Duration::from_millis(5));
        assert!(client_stats.total_pipeline_latency >= Duration::from_millis(5));
    }
}

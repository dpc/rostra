use rostra_core::id::RostraIdSecretKey;

use super::*;

#[tokio::test(start_paused = true)]
async fn retry_deadline_wakes_without_another_notification() {
    let sender = dedup_chan::Sender::<RostraId>::new();
    let mut receiver = sender.subscribe(1);
    let started_at = tokio::time::Instant::now();

    let reason =
        wait_for_retry_or_notification(&mut receiver, Some(started_at + Duration::from_secs(60)))
            .await;

    assert_eq!(reason, WakeReason::RetryDeadline);
    assert_eq!(started_at.elapsed(), Duration::from_secs(60));
}

#[tokio::test]
async fn lagged_notification_requests_durable_reconciliation() {
    let mut sender = dedup_chan::Sender::new();
    let mut receiver = sender.subscribe(1);
    sender.send(RostraIdSecretKey::from_bytes([41; 32]).id());
    sender.send(RostraIdSecretKey::from_bytes([42; 32]).id());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    assert!(matches!(
        wait_for_retry_or_notification(&mut receiver, Some(deadline)).await,
        WakeReason::Notification(_)
    ));
    assert_eq!(
        wait_for_retry_or_notification(&mut receiver, Some(deadline)).await,
        WakeReason::Lagging
    );
}

#[tokio::test(start_paused = true)]
async fn notification_does_not_move_existing_retry_deadline() {
    let mut sender = dedup_chan::Sender::new();
    let mut receiver = sender.subscribe(1);
    let started_at = tokio::time::Instant::now();
    let deadline = started_at + Duration::from_secs(60);
    sender.send(RostraIdSecretKey::from_bytes([43; 32]).id());

    assert!(matches!(
        wait_for_retry_or_notification(&mut receiver, Some(deadline)).await,
        WakeReason::Notification(_)
    ));
    assert_eq!(
        wait_for_retry_or_notification(&mut receiver, Some(deadline)).await,
        WakeReason::RetryDeadline
    );
    assert_eq!(started_at.elapsed(), Duration::from_secs(60));
}

#[tokio::test(start_paused = true)]
async fn retry_policy_reconciles_startup_lag_and_deadlines_through_cap() {
    let mut policy = MissingRetryPolicy::new();
    let mut reconciliations = 0;
    policy
        .reconcile(MissingReconcileTrigger::Startup, || {
            reconciliations += 1;
            async { Ok(MissingReconcileOutcome::WorkObserved) }
        })
        .await
        .expect("startup reconciliation");
    assert_eq!(reconciliations, 1);
    assert_eq!(policy.delay, INITIAL_RETRY_DELAY);
    let startup_deadline = policy.deadline.expect("startup retry deadline");

    policy
        .reconcile(MissingReconcileTrigger::Lag, || {
            reconciliations += 1;
            async { Ok(MissingReconcileOutcome::WorkObserved) }
        })
        .await
        .expect("lag reconciliation");
    assert_eq!(reconciliations, 2);
    assert_eq!(policy.delay, INITIAL_RETRY_DELAY);
    assert_eq!(policy.deadline, Some(startup_deadline));

    for _ in 0..16 {
        let previous_delay = policy.delay;
        let previous_reconciliations = reconciliations;
        policy
            .reconcile(MissingReconcileTrigger::RetryDeadline, || {
                reconciliations += 1;
                async { Ok(MissingReconcileOutcome::WorkObserved) }
            })
            .await
            .expect("deadline reconciliation");
        let expected_delay = previous_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
        assert_eq!(reconciliations, previous_reconciliations + 1);
        assert_eq!(policy.delay, expected_delay);
        assert_eq!(
            policy.deadline,
            Some(tokio::time::Instant::now() + expected_delay)
        );
    }
    assert_eq!(policy.delay, MAX_RETRY_DELAY);

    policy
        .reconcile(MissingReconcileTrigger::RetryDeadline, || async {
            Ok(MissingReconcileOutcome::Empty)
        })
        .await
        .expect("empty reconciliation");
    assert_eq!(policy.delay, INITIAL_RETRY_DELAY);
    assert!(policy.deadline.is_none());
}

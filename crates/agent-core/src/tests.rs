//! End-to-end tests against the real hub server on localhost.

use std::net::UdpSocket;
use std::sync::{Arc, Mutex};

use proto::descriptor::tests::BOOT_KEYBOARD;
use rawkvm_hub::config::HubConfig;
use rawkvm_hub::router::{Options, Router};
use rawkvm_hub::server;
use uuid::Uuid;

use super::*;

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Create(u8),
    Input(u8, Vec<u8>),
    DestroyAll,
    Ready,
    Activate,
    Deactivate,
}

#[derive(Clone, Default)]
struct RecSink(Arc<Mutex<Vec<Ev>>>, Duration);

impl RecSink {
    fn events(&self) -> Vec<Ev> {
        self.0.lock().unwrap().clone()
    }
}

impl HidSink for RecSink {
    fn create(&mut self, dev: &HidDevice) -> Result<()> {
        self.0.lock().unwrap().push(Ev::Create(dev.id));
        Ok(())
    }
    fn input(&mut self, dev_id: u8, report: &[u8]) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(Ev::Input(dev_id, report.to_vec()));
        Ok(())
    }
    fn destroy_all(&mut self) {
        self.0.lock().unwrap().push(Ev::DestroyAll);
    }
    fn devices_ready(&mut self) {
        self.0.lock().unwrap().push(Ev::Ready);
    }
    fn settle_time(&self) -> Duration {
        self.1
    }
    fn activate(&mut self, _: Option<Edge>, _: Option<f32>) {
        self.0.lock().unwrap().push(Ev::Activate);
    }
    fn deactivate(&mut self) {
        self.0.lock().unwrap().push(Ev::Deactivate);
    }
}

struct TestHub {
    router: Arc<Router>,
    cfg: HubConfig,
    dir: PathBuf,
    fingerprint: String,
}

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn start_hub() -> TestHub {
    let dir = std::env::temp_dir().join(format!("rawkvm-e2e-{}", Uuid::new_v4()));
    let mut cfg = HubConfig {
        listen: format!("127.0.0.1:{}", free_port()).parse().unwrap(),
        state_dir: dir.join("state"),
        mdns: false,
        ..Default::default()
    };
    cfg.complete();
    let id = server::load_or_create_identity(&cfg.state_dir).unwrap();
    let router = Arc::new(Router::new(
        cfg.hub_id,
        "test-hub".into(),
        Options {
            hysteresis: Duration::from_millis(150),
            resend: Some(Duration::from_millis(5)),
            hotkey: true,
            ..Default::default()
        },
    ));
    router.add_device(HidDevice {
        id: 0,
        name: "Logitech USB Receiver".into(),
        vid: 0x046d,
        pid: 0xc52b,
        serial: None,
        interface: 0,
        report_descriptor: BOOT_KEYBOARD.to_vec(),
        uses_report_ids: false,
    });
    let fingerprint = id.fingerprint.clone();
    let (c, r) = (cfg.clone(), router.clone());
    tokio::spawn(async move { server::serve(&c, &id, r).await.unwrap() });
    TestHub {
        router,
        cfg,
        dir,
        fingerprint,
    }
}

fn agent_config(hub: &TestHub, token: &str, pinned: &str) -> PathBuf {
    let path = hub.dir.join(format!("agent-{}.toml", Uuid::new_v4()));
    AgentConfig {
        node_id: Uuid::new_v4(),
        hub: hub.cfg.listen.to_string(),
        token: token.into(),
        hub_fingerprint: pinned.into(),
        ..Default::default()
    }
    .save(&path)
    .unwrap();
    path
}

async fn wait_for(sink: &RecSink, pred: impl Fn(&[Ev]) -> bool) {
    for _ in 0..200 {
        if pred(&sink.events()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out; events: {:?}", sink.events());
}

#[tokio::test]
async fn pairs_pins_and_receives_reports() {
    let hub = start_hub().await;
    let path = agent_config(&hub, &hub.cfg.token, "");
    let sink = RecSink::default();
    let mut agent_sink = sink.clone();
    let opts = AgentOptions {
        config_path: path.clone(),
        os: Os::Linux,
    };
    let test = async {
        wait_for(&sink, |e| {
            e.contains(&Ev::Activate) && e.contains(&Ev::Create(0))
        })
        .await;
        let pinned = AgentConfig::load_or_init(&path).unwrap().hub_fingerprint;
        assert_eq!(pinned, hub.fingerprint);

        let press = [0, 0, 0x04, 0, 0, 0, 0, 0];
        hub.router.on_report(0, &press);
        hub.router.on_report(0, &[0; 8]);
        // Wait past the redundant resend: it must be dropped as duplicate.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let inputs: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|e| matches!(e, Ev::Input(..)))
            .collect();
        assert_eq!(
            inputs,
            [Ev::Input(0, press.to_vec()), Ev::Input(0, vec![0; 8])]
        );
    };
    tokio::select! {
        r = run(opts, &mut agent_sink) => panic!("agent stopped: {r:?}"),
        _ = tokio::time::timeout(Duration::from_secs(10), test) => {}
    }
    let _ = std::fs::remove_dir_all(&hub.dir);
}

#[tokio::test]
async fn bad_token_is_fatal() {
    let hub = start_hub().await;
    let path = agent_config(&hub, &"00".repeat(32), "");
    let mut sink = RecSink::default();
    let opts = AgentOptions {
        config_path: path.clone(),
        os: Os::Linux,
    };
    let r = tokio::time::timeout(Duration::from_secs(10), run(opts, &mut sink))
        .await
        .expect("agent kept retrying");
    assert!(format!("{:#}", r.unwrap_err()).contains("token"));
    // Nothing gets pinned for a hub that did not accept us.
    assert!(AgentConfig::load_or_init(&path)
        .unwrap()
        .hub_fingerprint
        .is_empty());
    let _ = std::fs::remove_dir_all(&hub.dir);
}

#[tokio::test]
async fn changed_certificate_is_fatal() {
    let hub = start_hub().await;
    let path = agent_config(&hub, &hub.cfg.token, &"ab".repeat(32));
    let mut sink = RecSink::default();
    let opts = AgentOptions {
        config_path: path,
        os: Os::Linux,
    };
    let r = tokio::time::timeout(Duration::from_secs(10), run(opts, &mut sink))
        .await
        .expect("agent kept retrying");
    assert!(format!("{:#}", r.unwrap_err()).contains("hub_fingerprint"));
    let _ = std::fs::remove_dir_all(&hub.dir);
}

fn hid(id: u8) -> HidDevice {
    HidDevice {
        id,
        name: "Logitech USB Receiver".into(),
        vid: 0x046d,
        pid: 0xc52b,
        serial: None,
        interface: id,
        report_descriptor: BOOT_KEYBOARD.to_vec(),
        uses_report_ids: false,
    }
}

/// The hub announces the receiver's interfaces one by one, ~60 ms apart.
#[test]
fn incremental_device_lists_settle_into_one_ready() {
    let ms = Duration::from_millis;
    let mut sink = RecSink(Arc::default(), ms(500));
    let mut s = Session::new();
    let t0 = Instant::now();
    s.set_devices(vec![hid(0)], &mut sink, t0);
    s.set_devices(vec![hid(0), hid(1)], &mut sink, t0 + ms(60));
    s.poll_ready(&mut sink, t0 + ms(100));
    s.set_devices(vec![hid(0), hid(1), hid(2)], &mut sink, t0 + ms(120));
    s.poll_ready(&mut sink, t0 + ms(560)); // 500 ms after the first, not the last
    assert!(!sink.events().contains(&Ev::Ready));
    s.poll_ready(&mut sink, t0 + ms(620));
    s.poll_ready(&mut sink, t0 + ms(900));
    let ev = sink.events();
    assert_eq!(ev.iter().filter(|e| **e == Ev::Ready).count(), 1);
    // Ready comes after the last list's creates.
    assert_eq!(
        &ev[ev.len() - 4..],
        &[Ev::Create(0), Ev::Create(1), Ev::Create(2), Ev::Ready]
    );

    // A repeated list neither rebuilds nor re-arms the timer.
    s.set_devices(vec![hid(0), hid(1), hid(2)], &mut sink, t0 + ms(1000));
    assert!(s.ready_at.is_none());
}

#[test]
fn sinks_without_settle_time_are_ready_at_once() {
    let mut sink = RecSink::default();
    let mut s = Session::new();
    s.set_devices(vec![hid(0)], &mut sink, Instant::now());
    assert_eq!(sink.events(), [Ev::DestroyAll, Ev::Create(0), Ev::Ready]);
}

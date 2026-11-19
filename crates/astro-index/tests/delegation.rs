//! Cerveau partagé : une station confie l'inférence au service `ai-index` d'un ami, par une adresse locale (ici le service
//! lui-même ; en vrai, le `forward` du tunnel DRAGON). Indexeur factice : on teste le transport, les bornes et la confiance.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use astro_index::{IndexError, Indexer, Input, ModelTag, Output, RemoteIndexer, Task, server};
use bytes::Bytes;

struct Fake {
    calls: AtomicUsize,
    sha: &'static str,
    delay: Duration,
    vector: Vec<f32>,
}

impl Fake {
    fn new(sha: &'static str) -> Arc<Self> {
        Arc::new(Self { calls: AtomicUsize::new(0), sha, delay: Duration::ZERO, vector: vec![0.6, 0.8] })
    }
}

impl Indexer for Fake {
    fn tasks(&self) -> Vec<Task> {
        vec![Task::Vision]
    }
    async fn index(&self, _: Task, input: Input) -> Result<Output, IndexError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        assert!(!input.bytes.is_empty());
        Ok(Output { model: ModelTag { id: "siglip2-base".into(), sha256: self.sha.into() }, text: None, vector: Some(self.vector.clone()) })
    }
}

fn tag(sha: &str) -> ModelTag {
    ModelTag { id: "siglip2-base".into(), sha256: sha.into() }
}

fn photo(n: usize) -> Input {
    Input { mime: "image/jpeg".into(), bytes: Bytes::from(vec![7u8; n]) }
}

async fn remote(fake: Arc<Fake>, slots: usize) -> RemoteIndexer {
    let (addr, _) = server::spawn_loopback(fake, slots, Duration::from_secs(5), None).await.unwrap();
    RemoteIndexer::new(addr, "12D3KooWami", Duration::from_secs(5)).unwrap().expect(Task::Vision, tag("aa"))
}

#[tokio::test]
async fn a_friend_computes_the_vector_and_only_the_same_model_is_accepted() {
    let r = remote(Fake::new("aa"), 1).await;
    let out = r.index(Task::Vision, photo(100)).await.unwrap();
    assert_eq!((out.vector.unwrap(), r.peer(), r.tasks()), (vec![0.6, 0.8], "12D3KooWami", vec![Task::Vision]));

    let other = remote(Fake::new("bb"), 1).await;
    assert!(matches!(other.index(Task::Vision, photo(100)).await, Err(IndexError::Invalid(m)) if m.contains("incomparables")));
}

#[tokio::test]
async fn nothing_leaves_the_station_when_it_cannot_be_used() {
    let fake = Fake::new("aa");
    let r = remote(fake.clone(), 1).await;
    assert!(matches!(r.index(Task::Ocr, photo(10)).await, Err(IndexError::Unavailable(_))), "aucun modèle attendu");
    assert!(matches!(r.index(Task::Vision, photo(Task::Vision.max_input() + 1)).await, Err(IndexError::TooLarge(_))));
    assert!(r.index(Task::Vision, Input { mime: "application/x-sh".into(), bytes: Bytes::from_static(b"rm") }).await.is_err());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "rien n'a été envoyé");
    assert!(RemoteIndexer::new("192.168.1.9:80".parse().unwrap(), "p", Duration::from_secs(1)).is_err(), "boucle locale seulement");
}

#[tokio::test]
async fn a_busy_friend_queues_a_few_then_says_when_to_come_back_and_bad_results_never_leave_it() {
    let slow = Arc::new(Fake { calls: AtomicUsize::new(0), sha: "aa", delay: Duration::from_millis(300), vector: vec![1.0] });
    let r = remote(slow, 1).await;
    // Un en cours, deux en file (la part d'une station), le quatrième est renvoyé avec un délai de retour.
    let later = |ms| { let r = r.clone(); async move { tokio::time::sleep(Duration::from_millis(ms)).await; r.index(Task::Vision, photo(10)).await } };
    let (a, b, c, d) = tokio::join!(later(0), later(50), later(60), later(120));
    assert!(a.is_ok() && b.is_ok() && c.is_ok(), "mis en file, servis");
    assert!(matches!(d, Err(IndexError::Busy(s)) if (5..=600).contains(&s)), "file pleine : revenir plus tard");
    let load = r.load().await.unwrap();
    assert_eq!((load.slots, load.running, load.waiting), (1, 0, 0));
    assert!(load.avg_ms > 0 && load.avg_ms < 2000, "moyenne apprise : {} ms", load.avg_ms);

    let broken = Arc::new(Fake { calls: AtomicUsize::new(0), sha: "aa", delay: Duration::ZERO, vector: vec![f32::NAN] });
    let r = remote(broken, 1).await;
    assert!(matches!(r.index(Task::Vision, photo(10)).await, Err(IndexError::Backend(m)) if m.contains("500")));
}

#[tokio::test]
async fn a_hostile_friend_cannot_flood_or_fool_the_station() {
    use axum::routing::post;
    let app = axum::Router::new()
        .route("/v1/vision", post(|| async { "x".repeat(9 << 20) }))
        .route("/v1/text", post(|| async { r#"{"model":{"id":"siglip2-base","sha256":"aa"},"text":"je suis un vecteur"}"# }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let r = RemoteIndexer::new(addr, "p", Duration::from_secs(5)).unwrap().expect(Task::Vision, tag("aa")).expect(Task::Text, tag("aa"));
    assert!(matches!(r.index(Task::Vision, photo(10)).await, Err(IndexError::Backend(_))), "réponse de 9 Mo coupée");
    let text = Input { mime: "text/plain".into(), bytes: Bytes::from_static(b"facture") };
    assert!(matches!(r.index(Task::Text, text).await, Err(IndexError::Invalid(_))), "un texte au lieu d'un vecteur");
}

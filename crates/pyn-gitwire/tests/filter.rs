mod common;

use std::io::Read;

use common::*;
use pyn_gitwire::{FetchRequest, NoHaves, UploadPack};
use pyn_object::Algorithm;

#[test]
fn a_filter_is_advertised_only_when_allowed_and_sent_with_the_request() {
    let repo = init(Algorithm::Sha1, false);
    let tip = commit(repo.path(), "f.txt", "content");
    for (name, mut t) in shapes(repo.path()) {
        let up = UploadPack::open(t.as_mut(), Algorithm::Sha1).unwrap();
        assert!(!up.supports_filter(), "{name}");
    }
    git(repo.path(), &["config", "uploadpack.allowFilter", "true"]);
    for (name, mut t) in shapes(repo.path()) {
        let mut up = UploadPack::open(t.as_mut(), Algorithm::Sha1).unwrap();
        assert!(up.supports_filter(), "{name}");
        let mut req = FetchRequest::new(vec![tip]);
        req.filter = Some("blob:none".into());
        let mut pack = Vec::new();
        up.fetch(&req, &mut NoHaves, Box::new(|_| {}))
            .unwrap()
            .read_to_end(&mut pack)
            .unwrap();
        let scratch = init(Algorithm::Sha1, true);
        git_stdin(scratch.path(), &["index-pack", "--stdin"], &pack);
        let count = git(scratch.path(), &["count-objects", "-v"]);
        assert!(count.contains("in-pack: 2"), "{name}: {count}");
    }
}

#[test]
fn a_filter_the_server_lacks_is_refused_before_anything_is_sent() {
    let repo = init(Algorithm::Sha1, false);
    let tip = commit(repo.path(), "f.txt", "content");
    for (name, mut t) in shapes(repo.path()) {
        let mut up = UploadPack::open(t.as_mut(), Algorithm::Sha1).unwrap();
        let mut req = FetchRequest::new(vec![tip]);
        req.filter = Some("blob:none".into());
        let err = up.fetch(&req, &mut NoHaves, Box::new(|_| {})).err();
        assert!(
            matches!(err, Some(pyn_gitwire::Error::Unsupported(_))),
            "{name}"
        );
    }
}

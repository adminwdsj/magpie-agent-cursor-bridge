//! Maintains stable append-only Cursor root messages.
use crate::{cursor::checkpoint::messages, model::CanonicalMessage, store::BlobId, Error, Result};

use super::CheckpointBuilder;

#[derive(Clone)]
pub(super) struct RootFrontier {
    pub(super) ids: Vec<BlobId>,
    pub(super) generated: Vec<Vec<u8>>,
    pub(super) base_count: usize,
}

impl CheckpointBuilder {
    pub(super) async fn project_roots(
        &mut self,
        messages: &[CanonicalMessage],
    ) -> Result<Vec<BlobId>> {
        let wire_messages = messages::stable_messages(&self.instructions, messages, &self.model)?;
        self.ensure_roots()?;
        let replacement = self
            .roots
            .as_ref()
            .and_then(|roots| changed_system_root(roots, &wire_messages));
        if let Some(message) = replacement {
            let id = self.sync.persist(&message, &[]).await?;
            self.roots
                .as_mut()
                .ok_or_else(|| Error::Protocol("Cursor root frontier was not initialized".into()))?
                .ids[0] = id;
        }
        let roots = self
            .roots
            .as_mut()
            .ok_or_else(|| Error::Protocol("Cursor root frontier was not initialized".into()))?;
        if shrink_needs_replace(roots, wire_messages.len())? {
            tracing::warn!(
                cursor_roots = roots.ids.len(),
                local_roots = wire_messages.len(),
                "local history is shorter than Cursor's; publishing it as a replacement"
            );
            return self.replace_roots(messages).await;
        }
        for (index, expected) in roots.generated.iter().enumerate() {
            let wire_index = roots.base_count + index;
            if wire_messages.get(wire_index) != Some(expected) {
                return Err(Error::Protocol(format!(
                    "Cursor stable root changed at index {wire_index}"
                )));
            }
        }
        for message in wire_messages.iter().skip(roots.ids.len()) {
            roots.ids.push(self.sync.persist(message, &[]).await?);
            roots.generated.push(message.clone());
        }
        Ok(roots.ids.clone())
    }

    fn ensure_roots(&mut self) -> Result<()> {
        if self.roots.is_some() {
            return Ok(());
        }
        let ids = self
            .base
            .root_prompt_messages_json
            .iter()
            .map(|id| BlobId::from_bytes(id))
            .collect::<Result<Vec<_>>>()?;
        self.roots = Some(RootFrontier {
            base_count: ids.len(),
            ids,
            generated: Vec::new(),
        });
        Ok(())
    }

    pub(super) async fn replace_roots(
        &mut self,
        messages: &[CanonicalMessage],
    ) -> Result<Vec<BlobId>> {
        let wire_messages = messages::stable_messages(&self.instructions, messages, &self.model)?;
        self.ensure_roots()?;
        let previous_system = self
            .roots
            .as_ref()
            .and_then(|roots| roots.ids.first())
            .cloned();
        let mut ids = Vec::with_capacity(wire_messages.len());
        for (index, message) in wire_messages.iter().enumerate() {
            if index == 0
                && previous_system
                    .as_ref()
                    .is_some_and(|id| *id == BlobId::digest(message))
            {
                ids.push(previous_system.clone().expect("checked system root"));
            } else {
                ids.push(self.sync.persist(message, &[]).await?);
            }
        }
        self.roots = Some(RootFrontier {
            base_count: ids.len(),
            ids: ids.clone(),
            generated: Vec::new(),
        });
        Ok(ids)
    }
}

/// 本机历史比 Cursor 送来的 root 少时怎么办。
/// 本轮还没追加过 root：说明数据库头已经被压缩换掉，但压缩结果没来得及发给 Cursor（压缩后那一轮
/// 自己仍然超长失败，2026-11-14 22:11 实测 531→5、364→5）。以本机为准整体替换，和正常压缩发布同一条路。
/// 本轮已经追加过 root 还变短，是真正的状态错乱，照旧报错。
fn shrink_needs_replace(roots: &RootFrontier, wire_len: usize) -> Result<bool> {
    if wire_len >= roots.ids.len() {
        return Ok(false);
    }
    if roots.generated.is_empty() {
        return Ok(true);
    }
    Err(Error::Protocol(format!(
        "Cursor stable history shrank from {} to {} roots",
        roots.ids.len(),
        wire_len
    )))
}

fn changed_system_root(roots: &RootFrontier, messages: &[Vec<u8>]) -> Option<Vec<u8>> {
    roots
        .ids
        .first()
        .zip(messages.first())
        .filter(|(current, message)| **current != BlobId::digest(message))
        .map(|(_, message)| message.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontier(count: usize, generated: usize) -> RootFrontier {
        RootFrontier {
            ids: (0..count).map(|i| BlobId::digest(&[i as u8])).collect(),
            generated: vec![Vec::new(); generated],
            base_count: count - generated,
        }
    }

    #[test]
    fn locally_compacted_history_replaces_cursor_roots_instead_of_failing() {
        // 压缩后数据库只剩 5 条，Cursor 手里还是压缩前的 531 条，本轮还没追加 root。
        assert!(shrink_needs_replace(&frontier(531, 0), 5).unwrap());
        assert!(!shrink_needs_replace(&frontier(5, 0), 5).unwrap());
        assert!(!shrink_needs_replace(&frontier(5, 0), 7).unwrap());
        // 本轮自己追加过 root 之后又变短，是真错乱，必须报错。
        let error = shrink_needs_replace(&frontier(8, 2), 5).unwrap_err();
        assert!(error.to_string().contains("shrank from 8 to 5"), "{error}");
    }
}

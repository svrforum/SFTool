//! Backend-owned snapshots bind confirmation to the device the user saw.
use super::{model::DiskInfo, safety};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Selections {
    next: u64,
    disks: BTreeMap<String, DiskInfo>,
}

impl Selections {
    pub fn register(&mut self, disk: &DiskInfo) -> String {
        if let Some((id, _)) = self
            .disks
            .iter()
            .find(|(_, d)| d.number == disk.number && d.identity_key() == disk.identity_key())
        {
            return id.clone();
        }
        // Expired selections fail closed; a long-running window cannot grow forever.
        if self.disks.len() >= 512 {
            self.disks.clear();
        }
        self.next += 1;
        let id = self.next.to_string();
        self.disks.insert(id.clone(), disk.clone());
        id
    }

    pub fn selected(&self, id: &str) -> Result<DiskInfo, String> {
        self.disks.get(id).cloned().ok_or_else(|| {
            "장치 선택이 만료되었습니다. USB 목록을 새로고침하고 다시 선택해 주세요.".into()
        })
    }
}

pub fn resolve(selected: &DiskInfo, current: &[DiskInfo]) -> Result<DiskInfo, String> {
    let actual = current
        .iter()
        .find(|d| d.number == selected.number)
        .ok_or_else(|| "선택한 USB를 찾을 수 없습니다. 다시 선택해 주세요.".to_string())?;
    safety::confirm_identity(selected, actual).map_err(|e| e.describe())?;
    Ok(actual.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{fake::FakeEnumerator, UsbEnumerator};

    #[test]
    fn replacement_cannot_redefine_an_existing_selection() {
        let a = FakeEnumerator::sample().list_disks().unwrap()[2].clone();
        let mut b = a.clone();
        b.serial = Some("replacement".into());
        let mut selections = Selections::default();
        let old = selections.register(&a);
        assert_eq!(old, selections.register(&a));
        assert_ne!(old, selections.register(&b));
        assert!(resolve(&selections.selected(&old).unwrap(), &[b]).is_err());
        assert!(selections.selected("unknown").is_err());
    }
}

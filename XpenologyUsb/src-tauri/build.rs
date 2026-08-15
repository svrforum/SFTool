fn main() {
    // Windows 실행 파일에 관리자 권한 요청 매니페스트를 심는다.
    //
    // 이것이 없으면 \\.\PhysicalDriveN 을 여는 것 자체가 실패해서
    // 프로그램이 아무 일도 할 수 없다. 실행 시점에 권한을 확인하는 것으로는
    // 대체되지 않는다 — 권한 상승은 프로세스 시작 시에만 가능하다.
    let attrs = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("app.manifest")),
    );
    tauri_build::try_build(attrs).expect("tauri-build 실패");
}

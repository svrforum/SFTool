# Windows Defender 오탐 대응

`XpenologyUsb-portable.exe` 가 `Behavior:Win32/Persistence.A!ml` 로 잡히고, 내려받는
즉시 삭제되는 상태다. 이 문서는 그때 무엇을 해야 하는지 적어 둔 것이다.

## 왜 잡히는가

이 프로그램이 하는 일은 백신 입장에서 부트킷과 구분되지 않는다.

- 서명되지 않은 실행 파일이
- 관리자 권한을 요구하고
- `\\.\PhysicalDriveN` 을 열어 원시 섹터를 쓰고
- **디스크의 첫 섹터(파티션 테이블)를 덮어쓴다**

마지막 항목이 MITRE ATT&CK 의 T1542.003(Bootkit)과 같은 동작이다. 부트로더를 굽는
일 자체가 그것이므로 코드로 없앨 수 있는 부분이 아니다.

내려받자마자 삭제되는 것은 여기서 한 단계 더 간 상태다. 실행해 보고 판단하는 것이
아니라 **파일 자체가 차단**된 것이고, 클라우드 평판에 올라갔다는 뜻이다. 새로 빌드해
해시가 바뀌어도 같은 판정을 받을 수 있다.

## 지금 할 일 (순서대로)

### 1. Microsoft 에 오탐 신고

서명 없이 쓸 수 있는 유일한 실질적 해결책이다. 무료다.

https://www.microsoft.com/en-us/wdsi/filesubmission

- **Submission type**: Software developer
- **Detection name**: `Behavior:Win32/Persistence.A!ml`
- 파일을 올린다. 지금까지 배포한 버전을 **전부** 올리는 편이 낫다 — 판정이
  해시 하나가 아니라 비슷한 파일 전체에 걸려 있기 때문이다.
- "Do you believe this is a false positive?" → **Yes**

설명에 넣을 내용은 아래 [신고 문구](#신고-문구)에 있다.

회신까지 보통 며칠 걸린다. 해결되면 그 파일들은 통과하지만, **이후 새로 빌드한
파일은 다시 걸릴 수 있다.** 릴리스마다 다시 신고해야 할 수도 있다.

### 2. 사용자에게 알리기

배포처(포럼 글, 릴리스 노트)에 이 경고가 뜬다는 사실과 이유를 미리 적어 둔다.
아무 설명 없이 백신이 지우면 사용자는 프로그램이 실제로 악성이라고 판단한다.

### 3. 코드 서명 검토

근본 해결책이다. 비용이 들지만 이 문제가 반복되면 결국 이쪽이 싸다.
Azure Trusted Signing 이 개인 개발자에게 열려 있는지 확인해 볼 것 — 전통적인
인증서보다 훨씬 저렴하다.

## 코드로 할 수 있는 것과 없는 것

**없는 것.** 원시 디스크 쓰기와 첫 섹터 덮어쓰기는 이 프로그램의 기능 그 자체다.

**있는 것.** 점수를 더하는 군더더기를 만들지 않는 것.

- 0.4.4 에서 탐색기 창을 막으려고 `IOCTL_MOUNTMGR_SET_AUTO_MOUNT` 를 불렀다.
  이것은 `HKLM\SYSTEM\CurrentControlSet\Services\mountmgr\NoAutoMount` 에 쓰는,
  **프로세스가 끝난 뒤에도 남는 시스템 전역 변경**이다. 0.4.5 에서 되돌렸다.
- 실행 파일의 게시자·저작권·설명을 채워 둔다 (`tauri.conf.json` 의 `bundle`).
  비어 있으면 만든 사람을 알 수 없는 프로그램으로 보인다.

**새 Win32 호출을 넣을 때마다** "이게 프로세스 밖에 무언가를 남기는가" 를 먼저
묻는다. 남긴다면 그 기능은 이 프로그램에 넣지 않는 편이 낫다.

## 신고 문구

```
XpenologyUsb is an open-source utility that writes Xpenology bootloader
images (m-shell / RR) to a USB drive, in the same way as Rufus in DD mode
or Win32 Disk Imager.

It is detected as Behavior:Win32/Persistence.A!ml. We believe this is a
false positive caused by the nature of the tool rather than by anything
malicious:

- It opens \\.\PhysicalDriveN and writes raw sectors, because writing a
  bootable USB drive requires exactly that.
- It writes the image's MBR to sector 0, because a bootable drive needs a
  partition table. The application deliberately writes this last so that
  Windows cannot mount the volume mid-write.
- It requests administrator rights, because raw disk access requires them.
- It is not code signed. We are an individual developer.

The application does not persist anything on the system. It creates no
registry entries, services, scheduled tasks or startup items, and writes
no files outside the USB drive the user selected. The only thing that
survives the process exiting is the bootloader image on that drive.

Full source: https://github.com/svrforum/SFTool
The binaries are built from that source by GitHub Actions, and every
release publishes its SHA-256 hashes together with a link to the build
log, so any submitted file can be traced back to the exact commit it came
from.

Affected files (SHA-256):
  7afae2b777fa2da392ee0012a88e57480d2d231f66b31b5f0bbe7b5d78c2fd34  0.4.0
  4bf4acc7660c252a278fe7713d88ea900295473df33dfa83392afbb6871ff631  0.4.1
  efb1ff1fc96298854b90bbfd52e5a80ebed878123c6fe6756128cccd84b10b35  0.4.2
  f75fe4091b315998eb82d47ad76c98b339a68252873c788eb7d4f9ccb0e9b2ea  0.4.3
  57e14f659e942c990081682a84451048617d23c659d305155363e5a617deb242  0.4.4
```

## 사용자용 안내 문구

릴리스 노트와 배포 글에 넣을 것:

```
백신이 이 파일을 지울 수 있습니다

USB에 부트로더를 직접 굽는 프로그램이라, 백신 입장에서는 디스크의 첫 섹터를
건드리는 동작이 부트킷과 구분되지 않습니다. 여기에 코드 서명이 없어서
Windows Defender 가 차단하는 경우가 있습니다. Rufus 같은 도구도 서명을 받기
전에는 같은 문제를 겪었습니다.

Microsoft 에 오탐 신고를 해 둔 상태입니다.

이 프로그램은 시스템에 아무것도 남기지 않습니다. 레지스트리·서비스·시작
프로그램을 만들지 않고, 선택하신 USB 밖으로는 파일을 쓰지 않습니다.

소스는 전부 공개돼 있고 GitHub Actions 가 그 소스로 빌드합니다. 릴리스마다
SHA-256 해시와 빌드 기록 링크를 함께 올리니, 받으신 파일이 정말 그 소스에서
나온 것인지 직접 확인하실 수 있습니다.
```

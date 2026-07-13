## [0.2.0](https://github.com/madrigal-eschat/linux-game-haptics-router/compare/v0.1.2...v0.2.0) (2026-07-13)


### Features

* **app:** hold Plays that race ahead of their effect upload ([d1eda13](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/d1eda1361ea0fee07f9b2fc0ecb0eba172853866))
* **common:** add EVIOCRMFF support and tag ProbeEvent with a kind ([95d252e](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/95d252e263cb48d3becc0bdf9e353090a4dfd82b))
* **e2e:** add outer VM orchestration script ([779313d](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/779313d388142941ed78d7ab1d5518e0774521aa))
* **e2e:** add timing assertion logic for the 150ms bounds ([8b3d891](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/8b3d8915d4e60d83f3d9ac639365104662b81891))
* **e2e:** add virtual FF gamepad creation via uinput ([d9d3481](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/d9d3481556cf9a4778b992c6a2679800d1a66f42))
* **e2e:** define the smoke-set scenario data ([c86d433](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/c86d4339cb0d25a308818686702b14683b6c1cfa))
* **e2e:** implement in-process fake buttplug server ([05b1568](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/05b1568306fd08dbf9e782de3d6869b330df0203))
* **e2e:** implement the e2e-tests orchestrator binary ([e64b0b9](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/e64b0b9a911be70400858741b47b01537ebf047a))
* **e2e:** scaffold linux-game-haptics-router-e2e crate ([01c612e](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/01c612eae20c3556c0f5dc3e59b58f811e81a615))
* **ebpf-loader:** forward erase events alongside uploads ([5b6f65d](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/5b6f65de4799dd210e13c7ecdb0f893623e7d8aa))
* **ebpf:** capture EVIOCRMFF (effect erase) events ([fbf6ec8](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/fbf6ec804709764746be058c633f4498d66c5336))
* **main:** dispatch both upload and erase probe events ([74f588a](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/74f588ad852704fa2cab85b8a94beed2658ddad9))


### Bug Fixes

* **app:** resolve all pending plays sharing an effect_id; add PlaybackOps seam for testing ([5d00b0d](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/5d00b0ddc8e7502ca1cdcc94da0cbedf7485b8f1))
* **ci:** grant kvm device access before running e2e/run.sh ([a8f8281](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/a8f8281c78dab119532ea78b31adf9926ba244e9))
* **e2e:** advertise FF waveform bits, extend daemon warm-up delay ([7ccfe9f](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/7ccfe9f281edb259dd64e683f99cb0cd0069e163))
* **e2e:** apply cargo fmt and sync Cargo.lock with full workspace resolution ([6df67ae](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/6df67aef2aadffa030cb53e6e540073c0bff655e))
* **e2e:** drain daemon stderr concurrently to prevent pipe-buffer stall ([b3a176a](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/b3a176a5381b7bea50aa7518f7acff4aba9e3117))
* **e2e:** dump qemu.log on SSH-unreachable failure ([1f2da2b](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/1f2da2bd719414970303f5cf53b6787a60d9bc9c))
* **e2e:** emit a manual SYN_REPORT after every FF play write ([43f8ffc](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/43f8ffc6cc816af393b7b5fc956aa0043ca7566d))
* **e2e:** fall back to TCG on hosts without /dev/kvm, fix aarch64 machine type ([ca682c7](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/ca682c791085f8ef8087d38f340f8f9797af1b5a))
* **e2e:** guard daemon cleanup, fix spurious PASS, add retrigger/multi-device scenarios ([4503b6c](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/4503b6c6e07c112b6f5deb310a71dd727e8059b6))
* **e2e:** loosen timing bound to 250ms to absorb CI/VM scheduling jitter ([9ac734a](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/9ac734a708ffaed7119a458b8b2ca6b45da0044e))
* **e2e:** register cleanup trap before any early-exit path ([0f7a4f7](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/0f7a4f75f3e6896a70fb68d5c256ac0af0620294))
* **e2e:** use scp's -P (not ssh's -p) for the port flag ([c32b154](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/c32b154d896d9d2f8c897afc9bf1d4ddd6cd44c0))
* relax multi-device-resolve test's throttle-dependent assertion ([8a76bef](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/8a76bef0e5c22529f835994b869f6ad67fb093a6))
* rumble_effect test helper id must match the test's played effect_id ([2b11800](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/2b11800170fba6a03bce59db71aead3c85ea1925))
* scope HapticPoint import to test module in app.rs ([c1b953c](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/c1b953c977f212f28eef3465af5c4919a7661412))

## [0.1.2](https://github.com/madrigal-eschat/linux-game-haptics-router/compare/v0.1.1...v0.1.2) (2026-07-06)


### Bug Fixes

* **ci:** make codecov patch status informational ([2972791](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/2972791d6a78c81bd68069c39daff195751493b1))

## [0.1.1](https://github.com/madrigal-eschat/linux-game-haptics-router/compare/v0.1.0...v0.1.1) (2026-07-06)


### Bug Fixes

* **ci:** apply the E0152 exclude to pre-commit's local cargo-check hook too ([3421586](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/3421586f4c9537e20eac833d60c29b26879c6b80))
* **ci:** exclude ebpf crate from real-target builds, skip its cross-compile under coverage ([96abfd4](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/96abfd4d0a418d9fde5f5f0e5ba4d646d9eca684))
* **ci:** pin ambient toolchain to stable, exclude no_std ebpf crate from tests ([acdbeb9](https://github.com/madrigal-eschat/linux-game-haptics-router/commit/acdbeb9fa67fe3f2dee63de2ae99c73451d21be7))

## [1.8.1](https://github.com/bloop-box/bloop-server-framework/compare/v1.8.0...v1.8.1) (2025-12-10)


### Bug Fixes

* **evaluator:** consume player in streak evaluator instead of bloop ([2639f9c](https://github.com/bloop-box/bloop-server-framework/commit/2639f9c5a928a02f2653a590617ce7e7e6b5af52))

# [1.8.0](https://github.com/bloop-box/bloop-server-framework/compare/v1.7.0...v1.8.0) (2025-12-10)


### Features

* **evaluator:** rewrite streak evaluator and introduce distinct values evaluator ([e62cff3](https://github.com/bloop-box/bloop-server-framework/commit/e62cff375653151e3afb113c1f80f1decb14e70c))

# [1.7.0](https://github.com/bloop-box/bloop-server-framework/compare/v1.6.1...v1.7.0) (2025-12-09)


### Features

* **statistics:** accept server address as string ([369b415](https://github.com/bloop-box/bloop-server-framework/commit/369b415430d8ab269c9bdfb1c9b97b69ff7bc126))

## [1.6.1](https://github.com/bloop-box/bloop-server-framework/compare/v1.6.0...v1.6.1) (2025-12-09)


### Bug Fixes

* **statistics:** align server builder pattern with other builders ([add77b2](https://github.com/bloop-box/bloop-server-framework/commit/add77b2bad19540bbcb777a3f02a3805244163de))

# [1.6.0](https://github.com/bloop-box/bloop-server-framework/compare/v1.5.2...v1.6.0) (2025-12-09)


### Bug Fixes

* bump dependencies to latest ([3ead591](https://github.com/bloop-box/bloop-server-framework/commit/3ead5913f35acea545902afae2244cb27df2f51c))


### Features

* remove async_trait dep ([bcba7f7](https://github.com/bloop-box/bloop-server-framework/commit/bcba7f7a1c757098e3bc8d51edc12ca3e390520d))

## [1.5.2](https://github.com/bloop-box/bloop-server-framework/compare/v1.5.1...v1.5.2) (2025-12-09)


### Bug Fixes

* **engine:** require Default requirement for Metadata ([82ba219](https://github.com/bloop-box/bloop-server-framework/commit/82ba219b01c57630ba830793a70b321442a5d05f))

## [1.5.1](https://github.com/bloop-box/bloop-server-framework/compare/v1.5.0...v1.5.1) (2025-12-09)


### Bug Fixes

* **engine:** rename falsely named metadata to state ([731a2bd](https://github.com/bloop-box/bloop-server-framework/commit/731a2bd1b502501c09591aea5070ac2655710cbe))

# [1.5.0](https://github.com/bloop-box/bloop-server-framework/compare/v1.4.2...v1.5.0) (2025-12-09)


### Features

* **nfc_uid:** add serde support ([6fc1451](https://github.com/bloop-box/bloop-server-framework/commit/6fc14516b4e1e0f94bcc3349b267891544a2da9a))

## [1.4.2](https://github.com/bloop-box/bloop-server-framework/compare/v1.4.1...v1.4.2) (2025-12-08)


### Bug Fixes

* **semantic-release:** commit Cargo.lock on release ([cca0b15](https://github.com/bloop-box/bloop-server-framework/commit/cca0b15bef68460493f3d55ebc0fbe17f23298fd))

## [1.4.1](https://github.com/bloop-box/bloop-server-framework/compare/v1.4.0...v1.4.1) (2025-12-08)


### Bug Fixes

* **evaluator:** forbid duplicate players and self in streak checks ([fa366da](https://github.com/bloop-box/bloop-server-framework/commit/fa366da8a333701b5989b9532c1b6e148b4c15d7))

# [1.4.0](https://github.com/bloop-box/bloop-server-framework/compare/v1.3.3...v1.4.0) (2025-12-08)


### Features

* **evaluator:** add StreakEvaluator ([19ab2f4](https://github.com/bloop-box/bloop-server-framework/commit/19ab2f4b2b84427a0307e1faac9847934cc77257))

## [1.3.3](https://github.com/bloop-box/bloop-server-framework/compare/v1.3.2...v1.3.3) (2025-12-08)


### Bug Fixes

* **evaluator:** fix time DST handling ([592eb2d](https://github.com/bloop-box/bloop-server-framework/commit/592eb2d54db98cd3a2da2a3d70c998e5815e7389))

## [1.3.2](https://github.com/bloop-box/bloop-server-framework/compare/v1.3.1...v1.3.2) (2025-12-07)


### Bug Fixes

* rename missing Metadata to State ([65f377b](https://github.com/bloop-box/bloop-server-framework/commit/65f377bd8eb922550c31e46030cd54e7cfa6c79e))

## [1.3.1](https://github.com/bloop-box/bloop-server-framework/compare/v1.3.0...v1.3.1) (2025-12-07)


### Bug Fixes

* rename Metadata generic to State ([26f7426](https://github.com/bloop-box/bloop-server-framework/commit/26f742675a053d39d8bf57c15f12019451efd602))

# [1.3.0](https://github.com/bloop-box/bloop-server-framework/compare/v1.2.0...v1.3.0) (2025-12-07)


### Features

* **player:** add fn to PlayerRegistry to get player Arcs ([433ba26](https://github.com/bloop-box/bloop-server-framework/commit/433ba2648fa6f64700e5abb134d06817225e9458))

# [1.2.0](https://github.com/bloop-box/bloop-server-framework/compare/v1.1.0...v1.2.0) (2025-12-06)


### Features

* **nfc_uid:** add additional impls for vec handling ([648d882](https://github.com/bloop-box/bloop-server-framework/commit/648d882e7ebbe4ccad1ca568e9c340f72566944c))

# [1.1.0](https://github.com/bloop-box/bloop-server-framework/compare/v1.0.2...v1.1.0) (2025-12-05)


### Features

* make audio hash available through achievements ([41d3b4e](https://github.com/bloop-box/bloop-server-framework/commit/41d3b4eeb2b5c9313613f050f744c3af21c44016))

## [1.0.2](https://github.com/bloop-box/bloop-server-framework/compare/v1.0.1...v1.0.2) (2025-07-12)


### Bug Fixes

* rename features to capabilities ([56b987e](https://github.com/bloop-box/bloop-server-framework/commit/56b987e2df3345981c25ebdcb1f73cf0b258956b))

## [1.0.1](https://github.com/bloop-box/bloop-server-framework/compare/v1.0.0...v1.0.1) (2025-07-08)


### Bug Fixes

* **engine:** fix throttle logic ([9d93ce6](https://github.com/bloop-box/bloop-server-framework/commit/9d93ce6cb543ae94725847cdad940e5747fa8a67))

# 1.0.0 (2025-07-07)


### Features

* add health monitor and additional tests ([0991d5f](https://github.com/bloop-box/bloop-server-framework/commit/0991d5f625aca39f3e0b8ed9d227600d639d00d0))
* add statistics server feature ([f218bec](https://github.com/bloop-box/bloop-server-framework/commit/f218becda6be1ac753772513a5e8c656fc1a5c76))
* implement remaining 1.0 features ([806d0c1](https://github.com/bloop-box/bloop-server-framework/commit/806d0c168025408582f64ccd6257518f3edfa061))
* initial commit ([5cdb624](https://github.com/bloop-box/bloop-server-framework/commit/5cdb6244c2e30d86d56186b5a9e2978ba59924ff))

# CAM++ speaker embedding (3D-Speaker, zh-en)

- Architecture: CAM++ (~7M params), 192-dim speaker embedding, text-independent.
- Trained by the 3D-Speaker project (Alibaba DAMO / iic) on Chinese + English data
  (`iic/speech_campplus_sv_zh_en_16k-common_advanced`), Apache-2.0.
- ONNX conversion + metadata by sherpa-onnx (k2-fsa), Apache-2.0. Downloaded unchanged from the
  `speaker-recongition-models` release; sha256 pinned in `model.json`.
- Frontend expected by this file is documented in `model.json` and verified in
  `test/fixtures/speaker/golden/manifest.json`.
- Used here for on-device speaker verification only (enrollment centroid vs. utterance cosine).
  Embeddings are biometric data: keep them local, allow delete / re-enroll.

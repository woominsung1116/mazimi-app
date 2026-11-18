# AI 추출 평가 라벨셋

계약: [../../mazimi-ai-extraction.md](../../mazimi-ai-extraction.md) §10.

## 파일

| 파일 | 내용 |
|---|---|
| `export-label-candidates.sql` | 활성 공고 100건을 (source_type, program_type) 층화 라운드로빈으로 뽑아 라벨 템플릿 JSON을 만든다. 읽기 전용, 공개 공고 텍스트만 |
| `crates/worker/tests/fixtures/ai_extraction_sample.json` | SAMPLE 12건. 시드 데이터에서 에이전트가 초안 작성(`labeler: "draft-by-agent, needs human review"`, `human_verified: false`). 하네스·라벨 점검용이며 품질 수치로 쓰지 않는다 |

## 형식

```json
{
  "meta": {"labeler": "이름", "created_at": "YYYY-MM-DD", "schema_version": 1,
           "human_verified": false, "note": "선택"},
  "programs": [{
    "id": "program-<uuid>",
    "origin": "출처 설명(선택)",
    "label_note": "애매한 점·라벨하지 않은 이유(선택)",
    "source": {"title": "...", "summary": "...", "application_method": "...",
               "submission_documents": "...", "screening_method": "...", "raw_payload": {}},
    "coverage": {"age": false, "region": false, "enrollment": false,
                 "employment": false, "application_window": false, "documents": false},
    "expected": [
      {"fact": {"type": "age", "min_age": 19, "max_age": 34, "basis": "man_age"}, "quote": "만 19~34세"},
      {"fact": {"type": "income", "criterion": "income_decile", "comparator": "lte", "value": 4}, "quote": "소득 4분위 이하"},
      {"fact": {"type": "document", "name": "주민등록등본", "required": true, "notes": null}, "quote": "주민등록등본"},
      {"fact": {"type": "application_window", "kind": "dated", "start_date": "2026-03-01", "end_date": "2026-04-15"}, "quote": "2026.03.01 ~ 2026.04.15"}
    ]
  }]
}
```

- `fact`는 스키마 v1의 값 그대로다(`type` 태그 + 필드). 타입별 필드는 계약 §4.
- `coverage`는 수집 규칙이 이미 채운 타입이다(생략 = 전부 false). 켠 타입은 모델에 요청하지 않고 채점에서도 뺀다. 운영과 같게 평가하려면 DB 값을 보고 채운다.
- `expected`에 없는 타입은 "원문에 없음"이다. 모델이 그 타입을 내면 오탐(FP)으로 센다. 원문이 애매하면 라벨하지 말고 `label_note`에 적는다.
- `quote`는 **`--print-sources`가 출력한 마스킹된 원문**에서 그대로 복사한다(공백 포함). 원문 JSON에서 복사하면 마스킹·필드 제외 때문에 어긋날 수 있다.

## 라벨링 절차 (100건)

1. `psql "$DATABASE_URL" -At -v limit=100 -f docs/validation/ai-extraction/export-label-candidates.sql > label-candidates.json`
2. `cargo run -p mazimi-worker --bin extraction-eval -- --fixture label-candidates.json --print-sources > sources.txt`
3. 라벨러 A가 `expected`를 채운다. 사실은 원문이 직접 말하는 것만. 추론 금지(예: "대학생"을 재학으로 바꾸지 않음).
4. 형식·인용 점검: `cargo run -p mazimi-worker --bin extraction-eval -- --fixture label-candidates.json` (mock 오라클). FP·FN이 0이 아니면 인용이 원문에 없거나 가드를 통과하지 못한 라벨이다.
5. 라벨러 B가 원문과 대조해 검토하고 이견은 `label_note`에 남긴다. 둘 다 끝난 뒤에만 `human_verified: true`, `labeler`에 두 사람 이름.
6. 실제 모델 평가: 계약 §10의 `--provider ollama ... --gate`. 자동 반영을 켜려면 같은 실행에 `--auto-gate`를 더해 유형별 자동 검증 부분집합 precision ≥ 0.98(유형당 20건 이상)을 확인한다(계약 §14.5). 서류 라벨의 `required`도 정답 판정에 쓰이므로 정확히 적는다. 결과 JSON은 이 폴더에 날짜·모델·환경을 넣어 저장한다.

라벨셋은 모델·프롬프트 튜닝에 쓰지 않는다. 튜닝에 쓴 사례는 별도 dev 파일로 옮기고 측정용에서 뺀다(v1 계약 §2의 holdout 원칙과 같다).

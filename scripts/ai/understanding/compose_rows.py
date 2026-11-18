"""Compositional training rows, written fresh for round 2 of the
understanding model (never derived from dev, test or any acceptance row).

Each family pairs a request with its 1:1 negative built from the same
words (mention, question, retraction), so a word never decides the act on
its own. Families:
- elliptical and polite requests for alert on/off and save/unsave (the
  value heads' minority labels: off, unsave, polite on);
- corrections: a first clause, a correction marker, a final clause; the
  final clause decides the act and value, and "…, never mind" retracts;
- retractions after a request;
- "go apply" (the official page) against "I applied" (status) and a
  question about applying;
- a question about what a feature does is a question, never a card;
- a polite request phrased as a question ("…해 줄 수 있어?") is a request;
  a question about the state ("…돼 있어?") or a deliberation ("…할까
  말까") is a question;
- a write request followed by a read request, and a request for one type
  with a decline of the other;
- retraction words after a request ("…켜줘 장난이야") against affirming
  ones; set-aside ("X 말고 Y", "X 대신 Y": X retract, Y request) against
  "X 그리고 Y" (both asked); reported speech ("…켜 달래서 켰어", "…해 달라고
  했으니까 이제 Y": mention, then the request);
- the saved list and the selection as targets ("저장한 거 알림 다 꺼 줘"),
  with and without a programme on screen;
- polarity pairs: every verb family of unsave and alert-off (풀다, 해제,
  빼다, 내리다, 지우다; 끄다, 그만, 없애다, 멈추다) in plain, polite and
  polite-question forms plus a typo, with save and alert-on in the same
  frames;
- application outcomes: rejection ("떨어졌어", "불합격", "탈락") as the
  status value "rejected", against acceptance and application in the same
  frames.

Rows have the programme on screen and target on_screen (an objectless
write is about the programme on screen, as in the real rows), except the
list rows, whose context carries the saved list or the selection.
build_data.py applies the shared filters (sealed hashes, Jaccard against
dev and test, caps).
"""
import itertools, random

RNG = random.Random(20261029)
OBJECTS = ["이거", "이 공고", "이 혜택", "이 지원사업", "여기", "이 장학금", "이 프로그램"]


def cand(t, value, act, obj=True):
    # With the programme on screen an objectless write is about it too.
    del obj
    return {"type": t, "value": value, "target": "on_screen", "act": act}


# Request phrasings per (type, value), object first. "{o}" is the object.
REQUESTS = {
    ("alert", "on"): ["{o} 알림 켜주시겠어요", "{o} 알림 좀 켜 주실래요", "{o} 마감 알림 설정 부탁드려요",
                      "{o} 알림 받고 싶어요", "{o} 알림 걸어 주세요", "{o} 마감 전에 알려 주세요",
                      "{o} 알림 on", "{o} 알림 부탁해요", "{o} 마감 알림 받을게요", "{o} 리마인더 설정해 주세요",
                      "{o} 알림 신청할게요", "{o} 알림 설정 좀"],
    ("alert", "off"): ["{o} 알림 꺼주시겠어요", "{o} 알림 그만 받을게요", "{o} 알림 해제 부탁드려요",
                       "{o} 알림 off", "{o} 더는 알림 안 받을래요", "{o} 알림 끌게요",
                       "{o} 마감 알림 취소해 주세요", "{o} 알림 멈춰 주세요", "{o} 알림 이제 꺼도 돼요",
                       "{o} 알림 해제"],
    ("save", "save"): ["{o} 저장해 주시겠어요", "{o} 찜 부탁드려요", "{o} 관심 목록에 담아 주세요",
                       "{o} 북마크 해줘", "{o} 찜", "{o} 저장 좀요", "{o} 나중에 보게 저장해 둬",
                       "{o} 저장해 둘래요", "{o} 관심 등록해 주세요"],
    ("save", "unsave"): ["{o} 찜 해제해 주세요", "{o} 저장 취소해 주세요", "{o} 관심 목록에서 빼 주세요",
                         "{o} 저장한 거 지워 줘", "{o} 북마크 삭제", "{o} 찜 취소", "{o} 저장 목록에서 없애 줘",
                         "{o} 찜한 거 취소할게요", "{o} 이제 저장 안 해도 돼요", "{o} 관심 해제"],
}
# The same words in a non-request frame (the 1:1 negatives).
NEGATIVE_FRAMES = [("mention", "어제 {r}라고 했었지"), ("question", "{r} 하면 뭐가 달라져"),
                   ("mention", "친구는 {r}라고 하던데")]
# A polite request phrased as a question is still a request.
POLITE_QUESTION = {
    ("alert", "on"): ["{o} 알림 켜 줄 수 있어?", "{o} 알림 좀 걸어 줄래?", "{o} 마감 알림 설정해 줄 수 있을까요?"],
    ("alert", "off"): ["{o} 알림 꺼 줄 수 있어?", "{o} 알림 해제해 줄래요?", "{o} 알림 그만 오게 해 줄 수 있을까요?"],
    ("save", "save"): ["{o} 저장해 줄 수 있어?", "{o} 찜해 줄래?", "{o} 관심 목록에 넣어 줄 수 있을까요?"],
    ("save", "unsave"): ["{o} 찜 해제해 줄 수 있어?", "{o} 저장 취소해 줄래요?", "{o} 관심 목록에서 빼 줄 수 있을까요?"],
}
# Questions about the state, and deliberation: never a request.
STATE_QUESTIONS = {
    ("alert", "on"): ["{o} 알림 켜져 있어?", "{o} 알림 설정돼 있나요", "{o} 알림 지금 오는 중이야?",
                      "{o} 알림 켤까 고민돼", "{o} 알림 받는 게 나을까"],
    ("alert", "off"): ["{o} 알림 꺼져 있는 거 맞지", "{o} 알림 꺼 둔 상태야?", "{o} 알림 끌지 말지 모르겠어"],
    ("save", "save"): ["{o} 저장돼 있어?", "{o} 찜 되어 있나요", "{o} 저장했었나", "{o} 관심 목록에 들어가 있어?",
                       "{o} 저장할까 말까", "{o} 저장해 두는 게 좋을까"],
    ("save", "unsave"): ["{o} 찜 해제된 거 맞아?", "{o} 저장 취소할지 고민돼"],
}
# Retraction words after a request (the joke, the slip, the cancel): the
# final clause withdraws the request. Their 1:1 negative keeps the same
# request with an affirming tail.
RETRACTION_WORDS = ["장난이야", "농담이야", "농담", "장난", "뻥이야", "잘못 말했어", "방금 건 취소", "아니 됐다"]
AFFIRMING_WORDS = ["진짜야", "진심이야", "꼭 부탁해", "지금 바로"]
# Set-aside ("X 말고 Y", "X 대신 Y"): X is set aside (retract), Y is asked.
# The 1:1 negative joins both with "그리고": both asked.
SET_ASIDE_NOUN = {"alert": "알림", "save": "저장", "calendar": "캘린더", "form": "신청서 작성", "page": "공식 페이지"}
SET_ASIDE_REQUEST = {
    "alert": ("on", ["알림 켜줘", "마감 알림 걸어 줘"]),
    "save": ("save", ["저장해줘", "찜해 줘"]),
    "calendar": ("na", ["캘린더에 넣어줘", "달력에 마감일 표시해 줘"]),
    "form": ("na", ["신청서 작성 도와줘"]),
    "page": ("na", ["공식 신청 페이지 열어 줘"]),
}
SET_ASIDE_FRAMES = ["{nx} 말고 {ry}", "{nx} 대신 {ry}", "{rx} 대신에 {ry}", "{nx}는 됐고 {ry}", "{nx} 말고요 {ry}"]
# Reported speech: someone else's request, or the user's own earlier one,
# is mentioned, not asked; what follows "이제" is asked.
REPORTED = [("alert", "on", "알림 켜 달래서 켰어"), ("alert", "off", "알림 꺼 달래서 껐어"),
            ("save", "save", "저장해 달래서 저장했어"), ("save", "unsave", "찜 빼 달래서 뺐어"),
            ("alert", "on", "알림 켜 달라길래 켜 놨어"), ("save", "save", "저장하라고 해서 해 뒀어")]
SPEAKERS = ["엄마가", "친구가", "선배가", "담당자가", "동생이"]
EARLIER = {("save", "save"): "저장해 달라고 했으니까", ("alert", "on"): "알림 켜 달라고 했으니까",
           ("alert", "off"): "알림 꺼 달라고 했으니까", ("save", "unsave"): "찜 해제해 달라고 했으니까"}
NEXT = {("alert", "on"): "이제 알림도 켜 줘", ("save", "save"): "이제 저장도 해 줘",
        ("calendar", "na"): "이제 캘린더에도 넣어 줘"}
# Polarity pairs (batch 2: a typo of 해제 and a polite "풀어줄래?" carded
# the opposite direction). Every verb family of the minority direction
# (unsave: 풀다/해제/빼다/내리다/지우다; alert off: 끄다/해제/그만/없애다/멈추다)
# comes in plain, polite-question and polite forms, with the majority
# direction in the same frames so no frame decides the value; each phrase
# also gets one typo variant.
POLARITY = {
    ("save", "unsave"): ["찜 풀어줘", "찜 풀어 줄래?", "찜 좀 풀어 주세요", "저장 풀어 줘", "찜 해제해줘",
                         "저장 해제해 줄래?", "관심 해제해 주실래요?", "북마크 해제 부탁해요", "관심 목록에서 빼줘",
                         "저장 목록에서 빼 줄래?", "찜 목록에서 빼 주세요", "관심 목록에서 내려 줘",
                         "찜 목록에서 내려 줄래요?", "저장한 거 지워줘", "찜한 거 지워 줄래?", "북마크 지워 주세요"],
    ("save", "save"): ["찜해줘", "찜해 줄래?", "찜 좀 해 주세요", "저장해 줘", "저장해 줄래?",
                       "관심 등록해 주실래요?", "북마크 부탁해요", "관심 목록에 넣어줘", "저장 목록에 넣어 줄래?",
                       "찜 목록에 넣어 주세요", "관심 목록에 올려 줘", "찜 목록에 올려 줄래요?", "저장해 둬",
                       "찜해 둘래?", "북마크 해 주세요"],
    ("alert", "off"): ["알림 꺼줘", "알림 꺼 줄래?", "알림 좀 꺼 주실래요?", "알림 해제해줘", "알림 해제해 줄래?",
                       "마감 알림 해제 부탁해요", "알림 그만 보내 줘", "알림 그만 와도 돼", "알림 그만 받을래",
                       "알림 없애 줘", "알림 없애 줄래요?", "알림 멈춰 줘", "알림 중지해 줄래?"],
    ("alert", "on"): ["알림 켜줘", "알림 켜 줄래?", "알림 좀 켜 주실래요?", "알림 설정해줘", "알림 설정해 줄래?",
                      "마감 알림 설정 부탁해요", "알림 보내 줘", "알림 와도 돼", "알림 받을래", "알림 만들어 줘",
                      "알림 걸어 줄래요?", "알림 시작해 줘", "알림 다시 켜 줄래?"],
}
TYPO_SWAPS = [("해제", "헤제"), ("해제", "해재"), ("풀어", "풀러"), ("빼", "뺴"), ("꺼", "꺼"), ("켜", "켜"),
              ("지워", "지어"), ("저장", "저잔"), ("알림", "알람"), ("찜", "찡")]


def typo_of(g):
    """One keyboard-typo variant: a near-key or doubled-jamo swap, else no spaces."""
    hits = [(a, b) for a, b in TYPO_SWAPS if a in g and a != b]
    if hits:
        a, b = RNG.choice(hits)
        return g.replace(a, b, 1)
    return g.replace(" ", "")


# Rejection: a status the app does not record (core has no rejected
# status). The status head learns its own value, "rejected", which the
# policy turns into no card and the status picker; the other outcomes in
# the same frames keep their values.
OUTCOMES = {
    "rejected": ["{o} 떨어졌어", "{o} 불합격 나왔어요", "{o} 탈락했대", "{o} 선정 안 됐어요", "{o} 안 붙었어",
                 "{o} 떨어진 걸로 기록해 줘", "{o} 불합격으로 표시해 줄래?", "{o} 탈락으로 바꿔 주세요"],
    "received": ["{o} 붙었어", "{o} 합격 나왔어요", "{o} 선정됐대", "{o} 최종 합격했어요", "{o} 됐어 합격",
                 "{o} 합격한 걸로 기록해 줘", "{o} 합격으로 표시해 줄래?", "{o} 선정으로 바꿔 주세요"],
    "applied": ["{o} 지원했어", "{o} 서류 넣었어요", "{o} 신청 마쳤어", "{o} 접수했어요",
                "{o} 신청한 걸로 기록해 줘", "{o} 신청 완료로 표시해 줄래?"],
}


# A write request followed by a read request: the write is still asked.
READ_TAILS = ["그리고 마감일도 알려 줘", "비슷한 거 더 찾아 줘", "근데 서류는 뭐 필요해", "자격도 같이 봐 줘",
              "신청 방법도 알려 줄래"]
# A request for one type and a decline of the other.
DECLINE = {"alert": ["알림은 됐어", "알림은 필요 없어", "알림은 켜지 마"],
           "save": ["저장은 안 해도 돼", "찜은 하지 마", "저장은 됐어"]}
# The saved list and the selection as targets (programme not on screen).
SAVED_LISTS = ["저장한 거", "저장해 둔 공고들", "찜 목록에 있는 거", "북마크한 혜택들", "저장한 장학금 전부",
               "관심 목록 공고들"]
SELECTIONS = ["선택한 거", "고른 두 개", "체크해 둔 공고들", "골라 놓은 혜택들", "방금 고른 거"]
LIST_VERBS = {("alert", "on"): ["알림 다 켜 줘", "전부 마감 알림 걸어 주세요", "알림 일괄 설정"],
              ("alert", "off"): ["알림 다 꺼 줘", "알림 전부 해제해 주세요", "알림 끄기"],
              ("save", "save"): ["다 저장해 줘", "전부 찜해 주세요"],
              ("save", "unsave"): ["저장 다 취소해 줘", "찜 전부 해제"]}
RETRACT_TAILS = ["ㄴㄴ", "아 아니다 됐어", "취소할게", "아냐 하지 마", "없던 걸로 해 줘", "아 됐어요"]
CORRECTION_MARKS = ["아니", "아 아니", "아니 잠깐", "앗 아니다", "아니 그게 아니라", "정정할게요"]
FINAL = {
    ("alert", "on"): ["켜줘", "알림 켜 주세요", "켜는 걸로"], ("alert", "off"): ["꺼줘", "알림 꺼 주세요", "끄는 걸로"],
    ("save", "save"): ["저장해줘", "찜해 주세요"], ("save", "unsave"): ["저장 취소해줘", "찜 해제해 주세요"],
}
FIRST = {("alert", "on"): "알림 켜줘", ("alert", "off"): "알림 꺼줘",
         ("save", "save"): "저장해줘", ("save", "unsave"): "찜 해제해줘"}
OPPOSITE = {("alert", "on"): ("alert", "off"), ("alert", "off"): ("alert", "on"),
            ("save", "save"): ("save", "unsave"), ("save", "unsave"): ("save", "save")}
GO_APPLY = ["{o} 신청하러 갈게요", "{o} 신청 페이지 열어 줘", "{o} 지금 바로 신청할래", "{o} 공식 사이트에서 신청할게",
            "{o} 신청하러 가자", "{o} 신청 화면으로 가 줘"]
APPLIED = ["{o} 신청했어요", "{o} 방금 신청 완료했어", "{o} 신청서 냈어요", "{o} 접수 끝냈어요"]
APPLY_QUESTION = ["{o} 신청은 어디서 해", "{o} 신청하려면 어디로 가야 돼", "{o} 공식 페이지에서 신청하는 거 맞아"]
FEATURE_QUESTIONS = [
    ("form", "신청서 작성 도우미는 뭘 채워 줘"), ("form", "신청서 도우미가 어떤 칸까지 써 줘"),
    ("form", "신청서 미리 채우기는 무슨 정보로 해"), ("alert", "마감 알림은 며칠 전에 와"),
    ("alert", "알림 켜면 카톡으로 와 아니면 앱으로 와"), ("save", "저장하면 어디서 다시 봐"),
    ("calendar", "캘린더에 넣으면 구글 캘린더에도 떠"), ("status", "신청 상태 기록하면 누가 봐"),
    ("checklist", "체크리스트는 어떻게 체크해"), ("page", "공식 신청 페이지는 어디로 연결돼"),
]
FORM_REQUESTS = ["{o} 신청서 작성 도와줘", "{o} 신청서 미리 채워 줘", "{o} 신청서 쓰는 거 도와주세요"]


def row(goal, cands, family, k, ctx=None):
    return {"goal": " ".join(goal.split()), "ctx": ctx or {"has_program": True, "selected": 0, "saved": 0},
            "cands": cands, "source": "gen2", "group": f"gen2:{family}:{k}", "root": goal}


def rows():
    out = []
    k = itertools.count()
    for (t, v), phrases in REQUESTS.items():
        for p in phrases:
            for o in RNG.sample(OBJECTS, 2) + [""]:
                g = p.format(o=o)
                obj = bool(o)
                n = next(k)
                out.append(row(g, [cand(t, v, "request", obj)], "req", n))
                act, frame = NEGATIVE_FRAMES[n % len(NEGATIVE_FRAMES)]
                out.append(row(frame.format(r=g), [cand(t, v, act, obj)], "req", n))
            # retraction after the request
            o = RNG.choice(OBJECTS)
            n = next(k)
            out.append(row(f"{p.format(o=o)} {RNG.choice(RETRACT_TAILS)}", [cand(t, v, "retract")], "retract", n))
    for (t, v), phrases in POLITE_QUESTION.items():
        for p in phrases:
            for o in RNG.sample(OBJECTS, 2):
                out.append(row(p.format(o=o), [cand(t, v, "request")], "polite", next(k)))
    for (t, v), first in FIRST.items():
        for word in RETRACTION_WORDS:
            o = RNG.choice(OBJECTS + ["", ""])
            n = next(k)
            out.append(row(f"{o} {first} {word}", [cand(t, v, "retract")], "joke", n))
            out.append(row(f"{o} {first} {RNG.choice(AFFIRMING_WORDS)}", [cand(t, v, "request")], "joke", n))
    for x, y in itertools.permutations(SET_ASIDE_NOUN, 2):
        if {x, y} == {"form", "page"}:
            continue
        vx, rxs = SET_ASIDE_REQUEST[x]
        vy, rys = SET_ASIDE_REQUEST[y]
        for frame in RNG.sample(SET_ASIDE_FRAMES, 2):
            o = RNG.choice(OBJECTS + [""])
            n = next(k)
            g = frame.format(nx=SET_ASIDE_NOUN[x], rx=RNG.choice(rxs), ry=RNG.choice(rys))
            out.append(row(f"{o} {g}", [cand(x, vx, "retract"), cand(y, vy, "request")], "set_aside", n))
            both = f"{o} {RNG.choice(rxs)} 그리고 {RNG.choice(rys)}"
            out.append(row(both, [cand(x, vx, "request"), cand(y, vy, "request")], "set_aside", n))
    for t, v, phrase in REPORTED:
        for who in RNG.sample(SPEAKERS, 2):
            out.append(row(f"{who} {phrase}", [cand(t, v, "mention")], "reported", next(k)))
    for (t, v), earlier in EARLIER.items():
        for (ty, vy), nxt in NEXT.items():
            if ty == t:
                continue
            out.append(row(f"{earlier} {nxt}", [cand(t, v, "mention"), cand(ty, vy, "request")], "reported", next(k)))
    for value, phrases in OUTCOMES.items():
        for p in phrases:
            act = "request" if any(w in p for w in ("기록", "표시", "바꿔")) else "report"
            for o in RNG.sample(OBJECTS, 2):
                out.append(row(p.format(o=o), [cand("status", value, act)], "outcome", next(k)))
    for (t, v), phrases in POLARITY.items():
        for p in phrases:
            for o in RNG.sample(OBJECTS, 2):
                out.append(row(f"{o} {p}", [cand(t, v, "request")], "polarity", next(k)))
            out.append(row(f"{RNG.choice(OBJECTS)} {typo_of(p)}", [cand(t, v, "request")], "polarity", next(k)))
    for (t, v), phrases in STATE_QUESTIONS.items():
        for p in phrases:
            for o in RNG.sample(OBJECTS, 2):
                out.append(row(p.format(o=o), [cand(t, v, "question")], "state_q", next(k)))
    for (t, v), phrases in REQUESTS.items():
        for p in RNG.sample(phrases, 3):
            o = RNG.choice(OBJECTS)
            out.append(row(f"{p.format(o=o)} {RNG.choice(READ_TAILS)}", [cand(t, v, "request")], "then_read", next(k)))
    for (t, v), first in FIRST.items():
        other = "save" if t == "alert" else "alert"
        for d in DECLINE[other]:
            o = RNG.choice(OBJECTS)
            n = next(k)
            ov = "on" if other == "alert" else "save"
            out.append(row(f"{o} {first} {d}", [cand(t, v, "request"), cand(other, ov, "retract")], "decline", n))
    for (t, v), verbs in LIST_VERBS.items():
        for verb in verbs:
            lists = [("saved", RNG.choice(SAVED_LISTS))]
            if t == "alert" or v == "save":
                lists.append(("selected", RNG.choice(SELECTIONS)))
            for target, noun in lists:
                ctx = {"has_program": False, "selected": 2 if target == "selected" else 0,
                       "saved": 3 if target == "saved" else 0}
                c = dict(cand(t, v, "request"), target=target)
                out.append(row(f"{noun} {verb}", [c], "list", next(k), ctx))
                # the same list with a programme on screen is still the list
                on = dict(ctx, has_program=True)
                out.append(row(f"{RNG.choice(SAVED_LISTS if target == 'saved' else SELECTIONS)} {verb}", [c],
                               "list", next(k), on))
    for (t, v), first in FIRST.items():
        ot, ov = OPPOSITE[(t, v)]
        for mark in CORRECTION_MARKS:
            o = RNG.choice(OBJECTS)
            n = next(k)
            final = RNG.choice(FINAL[(ot, ov)])
            # the final clause decides: the opposite value is the request
            out.append(row(f"{o} {first} {mark} {final}", [cand(ot, ov, "request")], "correct", n))
            # its negative: the correction withdraws everything
            out.append(row(f"{o} {first} {mark} 됐어", [cand(t, v, "retract")], "correct", n))
    for p in GO_APPLY:
        for o in RNG.sample(OBJECTS, 2):
            n = next(k)
            out.append(row(p.format(o=o), [cand("page", "na", "request")], "apply", n))
            q = RNG.choice(APPLY_QUESTION).format(o=o)
            out.append(row(q, [cand("page", "na", "question")], "apply", n))
    for p in APPLIED:
        for o in RNG.sample(OBJECTS, 2):
            out.append(row(p.format(o=o), [cand("status", "applied", "report")], "applied", next(k)))
    for t, q in FEATURE_QUESTIONS:
        for o in ["", RNG.choice(OBJECTS)]:
            out.append(row(f"{o} {q}", [cand(t, "na", "question", bool(o))], "feature_q", next(k)))
    for p in FORM_REQUESTS:
        for o in RNG.sample(OBJECTS, 2):
            out.append(row(p.format(o=o), [cand("form", "na", "request")], "form", next(k)))
    return out


if __name__ == "__main__":
    import collections, json
    rs = rows()
    print(len(rs), collections.Counter((c["type"], c["act"]) for r in rs for c in r["cands"]))
    for r in rs[::25]:
        print(r["goal"], r["cands"])

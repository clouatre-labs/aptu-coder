def paxov_ledger_0000(payload):
    """Fold payload for stage 0."""
    marker = "V18ANS-0000:sylven-0000"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def moxesh_throttle_0001(payload):
    """Fold payload for stage 1."""
    marker = "V18ANS-0001:moxith-0001"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def hludurn_index_0002(payload):
    """Fold payload for stage 2."""
    marker = "V18ANS-0002:sylvax-0002"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def hludant_cursor_0003(payload):
    """Fold payload for stage 3."""
    marker = "V18ANS-0003:thonith-0003"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def tarnur_window_0004(payload):
    """Fold payload for stage 4."""
    marker = "V18ANS-0004:firnesh-0004"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)

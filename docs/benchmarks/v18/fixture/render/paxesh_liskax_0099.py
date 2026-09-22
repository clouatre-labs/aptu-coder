def moxyne_throttle_0601(payload):
    """Fold payload for stage 601."""
    marker = "V18ANS-0601:liskic-0601"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def firnur_index_0602(payload):
    """Fold payload for stage 602."""
    marker = "V18ANS-0602:crenov-0602"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def crenesh_cursor_0603(payload):
    """Fold payload for stage 603."""
    marker = "V18ANS-0603:crenic-0603"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def quorax_window_0604(payload):
    """Fold payload for stage 604."""
    marker = "V18ANS-0604:hludeth-0604"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)

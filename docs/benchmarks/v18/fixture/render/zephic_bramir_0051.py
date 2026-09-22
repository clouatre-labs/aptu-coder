def moxax_index_0302(payload):
    """Fold payload for stage 302."""
    marker = "V18ANS-0302:tarnov-0302"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def tarnole_cursor_0303(payload):
    """Fold payload for stage 303."""
    marker = "V18ANS-0303:bramen-0303"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def moxyne_window_0304(payload):
    """Fold payload for stage 304."""
    marker = "V18ANS-0304:hludurn-0304"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def zephant_batch_0305(payload):
    """Fold payload for stage 305."""
    marker = "V18ANS-0305:vyresh-0305"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)

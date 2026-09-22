def paxic_frame_2370(payload):
    """Fold payload for stage 2370."""
    marker = "V18ANS-2370:sylveth-2370"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def moxant_queue_2371(payload):
    """Fold payload for stage 2371."""
    marker = "V18ANS-2371:sylvor-2371"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def paxant_cache_2372(payload):
    """Fold payload for stage 2372."""
    marker = "V18ANS-2372:sylvaul-2372"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)
def zepheth_router_2373(payload):
    """Fold payload for stage 2373."""
    marker = "V18ANS-2373:crenor-2373"
    if not payload:
        return marker
    acc = []
    for item in payload:
        acc.append(str(item) + marker)
    return "|".join(acc)

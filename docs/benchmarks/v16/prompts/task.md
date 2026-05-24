## Task: OpenFAST AeroDyn Integration Point Audit

You are onboarding to the OpenFAST codebase to extend the AeroDyn module with a new blade-load
correction factor. OpenFAST is a physics-based wind turbine simulation framework. AeroDyn is the
aerodynamics module responsible for computing blade loads. It follows the FAST modular framework
interface: every physics module exposes `Init`, `CalcOutput`, `UpdateStates`, and `End` subroutines
with module-prefixed names (e.g., `AD_CalcOutput`).

Your task:
1. Identify the exact Fortran files and subroutine names (with line numbers) that define AeroDyn's
   top-level `CalcOutput` and `UpdateStates` routines. These are named `AD_CalcOutput` and
   `AD_UpdateStates` (module-prefixed) and live in `modules/aerodyn/src/AeroDyn.f90`.
2. Trace which NWTC library routines (from `modules/nwtc-library/src/`) are called within
   `AD_CalcOutput` -- up to 2 call levels deep. For each routine, name it and the file it is defined in.
3. Identify which derived types (Fortran `TYPE` definitions) from `modules/nwtc-library/src/` are
   used by AeroDyn. For each type, name the file and approximate line number where it is declared.
4. Produce an integration map: which files and line ranges must change to inject a new scalar
   correction-factor parameter (`BladeLoadCorrFactor`) through the call stack -- from the glue-code
   entry point in `modules/openfast-library/src/FAST_Subs.f90` down to the blade-element calculation
   inside AeroDyn.

Output must be valid JSON. Example structure:
```json
{
  "run_id": "RUN_ID_PLACEHOLDER",
  "condition": "CONDITION_PLACEHOLDER",
  "aerodyn_entry_points": [
    {"subroutine": "AD_CalcOutput", "file": "path/relative/to/openfast/root", "line": 0},
    {"subroutine": "AD_UpdateStates", "file": "path/relative/to/openfast/root", "line": 0}
  ],
  "nwtc_callees": [
    {"routine": "name", "file": "path/relative/to/openfast/root", "call_depth": 1},
    {"routine": "name2", "file": "path/relative/to/openfast/root", "call_depth": 2}
  ],
  "nwtc_types_used": [
    {"type_name": "TypeName", "declared_in": "path/relative/to/openfast/root", "approx_line": 0}
  ],
  "integration_map": [
    {"file": "path/relative/to/openfast/root", "line_range": "start-end", "change": "description"}
  ],
  "tool_calls_total": 0
}
```

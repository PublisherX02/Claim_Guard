rule E001_fail {
  meta:
    rule_id = "E001"
    outcome = "FAIL"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = /E001:FAIL:/
  condition:
    $m
}

rule E001_pass {
  meta:
    rule_id = "E001"
    outcome = "PASS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E001:OK"
  condition:
    $m
}

rule E001_unable_to_assess {
  meta:
    rule_id = "E001"
    outcome = "UNABLE_TO_ASSESS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E001:UNKNOWN"
  condition:
    $m
}

rule E001_not_applicable {
  meta:
    rule_id = "E001"
    outcome = "NOT_APPLICABLE"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E001:NA"
  condition:
    $m
}

rule E002_fail {
  meta:
    rule_id = "E002"
    outcome = "FAIL"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = /E002:FAIL:/
  condition:
    $m
}

rule E002_pass {
  meta:
    rule_id = "E002"
    outcome = "PASS"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = "E002:OK"
  condition:
    $m
}

rule E002_unable_to_assess {
  meta:
    rule_id = "E002"
    outcome = "UNABLE_TO_ASSESS"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = "E002:UNKNOWN"
  condition:
    $m
}

rule E002_not_applicable {
  meta:
    rule_id = "E002"
    outcome = "NOT_APPLICABLE"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = "E002:NA"
  condition:
    $m
}

rule E003_fail {
  meta:
    rule_id = "E003"
    outcome = "FAIL"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = /E003:FAIL:/
  condition:
    $m
}

rule E003_pass {
  meta:
    rule_id = "E003"
    outcome = "PASS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E003:OK"
  condition:
    $m
}

rule E003_unable_to_assess {
  meta:
    rule_id = "E003"
    outcome = "UNABLE_TO_ASSESS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E003:UNKNOWN"
  condition:
    $m
}

rule E003_not_applicable {
  meta:
    rule_id = "E003"
    outcome = "NOT_APPLICABLE"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E003:NA"
  condition:
    $m
}

rule E004_fail {
  meta:
    rule_id = "E004"
    outcome = "FAIL"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = /E004:FAIL:/
  condition:
    $m
}

rule E004_pass {
  meta:
    rule_id = "E004"
    outcome = "PASS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E004:OK"
  condition:
    $m
}

rule E004_unable_to_assess {
  meta:
    rule_id = "E004"
    outcome = "UNABLE_TO_ASSESS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E004:UNKNOWN"
  condition:
    $m
}

rule E004_not_applicable {
  meta:
    rule_id = "E004"
    outcome = "NOT_APPLICABLE"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E004:NA"
  condition:
    $m
}

rule E005_fail {
  meta:
    rule_id = "E005"
    outcome = "FAIL"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = /E005:FAIL:/
  condition:
    $m
}

rule E005_pass {
  meta:
    rule_id = "E005"
    outcome = "PASS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E005:OK"
  condition:
    $m
}

rule E005_unable_to_assess {
  meta:
    rule_id = "E005"
    outcome = "UNABLE_TO_ASSESS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E005:UNKNOWN"
  condition:
    $m
}

rule E005_not_applicable {
  meta:
    rule_id = "E005"
    outcome = "NOT_APPLICABLE"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E005:NA"
  condition:
    $m
}

rule E101_fail {
  meta:
    rule_id = "E101"
    outcome = "FAIL"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = /E101:FAIL:/
  condition:
    $m
}

rule E101_pass {
  meta:
    rule_id = "E101"
    outcome = "PASS"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = "E101:OK"
  condition:
    $m
}

rule E101_unable_to_assess {
  meta:
    rule_id = "E101"
    outcome = "UNABLE_TO_ASSESS"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = "E101:UNKNOWN"
  condition:
    $m
}

rule E101_not_applicable {
  meta:
    rule_id = "E101"
    outcome = "NOT_APPLICABLE"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = "E101:NA"
  condition:
    $m
}

rule E102_fail {
  meta:
    rule_id = "E102"
    outcome = "FAIL"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = /E102:FAIL:/
  condition:
    $m
}

rule E102_pass {
  meta:
    rule_id = "E102"
    outcome = "PASS"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = "E102:OK"
  condition:
    $m
}

rule E102_unable_to_assess {
  meta:
    rule_id = "E102"
    outcome = "UNABLE_TO_ASSESS"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = "E102:UNKNOWN"
  condition:
    $m
}

rule E102_not_applicable {
  meta:
    rule_id = "E102"
    outcome = "NOT_APPLICABLE"
    severity = "high"
    rule_version = "1.0.0"
  strings:
    $m = "E102:NA"
  condition:
    $m
}

rule E103_fail {
  meta:
    rule_id = "E103"
    outcome = "FAIL"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = /E103:FAIL:/
  condition:
    $m
}

rule E103_pass {
  meta:
    rule_id = "E103"
    outcome = "PASS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E103:OK"
  condition:
    $m
}

rule E103_unable_to_assess {
  meta:
    rule_id = "E103"
    outcome = "UNABLE_TO_ASSESS"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E103:UNKNOWN"
  condition:
    $m
}

rule E103_not_applicable {
  meta:
    rule_id = "E103"
    outcome = "NOT_APPLICABLE"
    severity = "medium"
    rule_version = "1.0.0"
  strings:
    $m = "E103:NA"
  condition:
    $m
}

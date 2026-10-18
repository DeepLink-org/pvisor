"""Capacity labels and summed counters cannot replace actual per-owner settings."""

import copy
import json
import unittest

from filesystem_cache_capacity import (
    CAPACITY_ENV,
    DATA_COUNTER,
    DATA_ENV,
    FACTORIAL_DESIGN,
    FLUSH_COUNTER,
    FLUSH_ENV,
    NEGATIVE_COUNTER,
    NEGATIVE_ENTRY_COUNTER,
    NEGATIVE_ENTRY_DESIGN,
    NEGATIVE_ENTRY_ENV,
    NEGATIVE_ENTRY_REPLIES,
    NEGATIVE_ENV,
    QUALIFICATION_COUNTERS,
    SHARED_OPEN_COUNTER,
    SHARED_OPEN_DESIGN,
    SHARED_OPEN_ENV,
    UPPER_COUNTER,
    UPPER_ENV,
    capacity_conditions,
    capacity_environment,
    validate_profile_capacity,
    validate_profile_factorial_settings,
    validate_profile_lower_data_cache,
    validate_profile_negative_cache,
    validate_profile_negative_entry_settings,
    validate_profile_readonly_flush_elision,
    validate_profile_shared_open_settings,
    validate_profile_upper_absence_cache,
    validate_shared_open_inventory,
)


def profiles(capacity):
    rows, final = [], []
    for instance, scope in enumerate(("rootfs", "pvisor-workspace"), 1):
        rows.append(dict(pid=100, component="virtio-fs-overlay", instance=instance, scope=scope))
        final.append(
            dict(
                pid=100,
                component="virtio-fs-overlay",
                instance=instance,
                final_record=True,
                measurements={"directory_cache_capacity": {"units": capacity}},
            )
        )
    return dict(rows=rows, final_profiles=final)


def factorial_config():
    return dict(
        owned_inputs_manifest="/sealed/inputs.json",
        treatment_design=FACTORIAL_DESIGN,
        variants=[
            dict(name=name, capacity=256, env={DATA_ENV: data, FLUSH_ENV: flush})
            for name, data, flush in (
                ("neither", "0", "0"),
                ("data", "1", "0"),
                ("flush", "0", "1"),
                ("both", "1", "1"),
            )
        ],
    )


def factorial_profiles(environment):
    value = profiles(256)
    for final in value["final_profiles"]:
        final["measurements"].update(
            {
                DATA_COUNTER: {"units": int(environment[DATA_ENV])},
                FLUSH_COUNTER: {"units": int(environment[FLUSH_ENV])},
            }
        )
    for instance, scope in enumerate(("rootfs", "pvisor-workspace"), 10):
        value["rows"].append(
            dict(pid=100, component="overlay-core", instance=instance, scope=scope)
        )
        value["final_profiles"].append(
            dict(
                pid=100,
                component="overlay-core",
                instance=instance,
                final_record=True,
                measurements={NEGATIVE_COUNTER: {"units": 0}, UPPER_COUNTER: {"units": 0}},
            )
        )
    return value


def negative_entry_config():
    return dict(
        owned_inputs_manifest="/sealed/inputs.json",
        treatment_design=NEGATIVE_ENTRY_DESIGN,
        variants=[
            dict(name="entry-off", capacity=256, env={NEGATIVE_ENTRY_ENV: "0"}),
            dict(name="entry-on", capacity=256, env={NEGATIVE_ENTRY_ENV: "1"}),
        ],
    )


def negative_entry_profiles(environment, replies=0):
    value = factorial_profiles(environment)
    for final in value["final_profiles"][:2]:
        final["measurements"].update(
            {
                NEGATIVE_ENTRY_COUNTER: {"units": int(environment[NEGATIVE_ENTRY_ENV])},
                NEGATIVE_ENTRY_REPLIES: {"units": replies},
            }
        )
    return value


class CacheCapacityTests(unittest.TestCase):
    def variants(self):
        return dict(
            owned_inputs_manifest="/sealed/inputs.json",
            variants=[
                dict(name="negative-off", capacity=256, env={NEGATIVE_ENV: "0"}),
                dict(name="negative-on", capacity=256, env={NEGATIVE_ENV: "1"}),
            ],
        )

    def test_capacity_arms_are_explicit_unique_bounded_integers(self):
        self.assertEqual(
            capacity_conditions({"capacities": [256, 1024]}),
            [("capacity-256", 256), ("capacity-1024", 1024)],
        )
        for values in ([], [256], [256, 256], [0, 1024], [256, 4097], [True, 1024], ["256", 1024]):
            with self.subTest(values=values), self.assertRaises(ValueError):
                capacity_conditions({"capacities": values})

    def test_timing_and_profile_modes_keep_guest_and_ownership_controls(self):
        config = {"owned_inputs_manifest": "/sealed/inputs.json"}
        timing = capacity_environment(config, 256, "timing")
        diagnostic = capacity_environment(config, 1024, "profiles")
        self.assertEqual(timing["PVISOR_FS_PROFILE"], "0")
        self.assertEqual(diagnostic["PVISOR_FS_PROFILE"], "1")
        self.assertEqual(timing[CAPACITY_ENV], "256")
        self.assertEqual(diagnostic[CAPACITY_ENV], "1024")
        for environment in (timing, diagnostic):
            self.assertEqual(environment["PVISOR_FS_OWNED_INPUTS_CONDITION"], "both")
            self.assertEqual(environment["PVISOR_VM_FS_WRITEBACK"], "0")
            self.assertEqual(environment["PVISOR_VM_GUEST_CACHE"], "1")

    def test_actual_capacity_requires_each_mounted_final_and_does_not_sum(self):
        valid = profiles(1024)
        # Other registered owners may exist outside these mounted devices.
        valid["final_profiles"].append(
            dict(
                pid=100,
                component="virtio-fs-overlay",
                instance=99,
                final_record=True,
                measurements={"directory_cache_capacity": {"units": 256}},
            )
        )
        self.assertEqual(len(validate_profile_capacity(valid, 1024)), 2)
        for fault in ("summed", "missing_scope", "missing_counter", "unfinished"):
            with self.subTest(fault=fault):
                changed = copy.deepcopy(valid)
                if fault == "summed":
                    # Two 512-capacity owners do not prove one 1024-capacity arm.
                    for profile in changed["final_profiles"][:2]:
                        profile["measurements"]["directory_cache_capacity"]["units"] = 512
                elif fault == "missing_scope":
                    changed["rows"].pop()
                elif fault == "missing_counter":
                    changed["final_profiles"][0]["measurements"].clear()
                else:
                    changed["final_profiles"][0]["final_record"] = False
                with self.assertRaises(ValueError):
                    validate_profile_capacity(changed, 1024)

    def test_variants_allow_equal_capacity_but_reject_path_and_control_changes(self):
        self.assertEqual(
            capacity_conditions(self.variants()), [("negative-off", 256), ("negative-on", 256)]
        )
        for name in (
            "",
            "../escape",
            "/absolute",
            "with/slash",
            "with.dot",
            "x" * 65,
            "negative-off",
        ):
            config = self.variants()
            config["variants"][1]["name"] = name
            with self.subTest(name=name), self.assertRaises(ValueError):
                capacity_conditions(config)
        for environment in (
            {},
            {NEGATIVE_ENV: "2"},
            {NEGATIVE_ENV: 1},
            {"PVISOR_VM_FS_WRITEBACK": "0"},
            {NEGATIVE_ENV: "1", "PVISOR_FS_PROFILE": "0"},
        ):
            config = self.variants()
            config["variants"][1]["env"] = environment
            with self.subTest(environment=environment), self.assertRaises(ValueError):
                capacity_conditions(config)
        for capacity in (0, 4097, True, "256"):
            config = self.variants()
            config["variants"][1]["capacity"] = capacity
            with self.subTest(capacity=capacity), self.assertRaises(ValueError):
                capacity_conditions(config)

    def test_variant_environment_selects_the_named_arm_without_changing_ownership(self):
        environment = capacity_environment(self.variants(), 256, "profiles", "negative-on")
        self.assertEqual(environment[NEGATIVE_ENV], "1")
        self.assertEqual(environment[CAPACITY_ENV], "256")
        self.assertEqual(environment["PVISOR_FS_OWNED_INPUTS_CONDITION"], "both")
        self.assertEqual(environment["PVISOR_VM_FS_WRITEBACK"], "0")
        with self.assertRaises(ValueError):
            capacity_environment(self.variants(), 256, "timing")
        with self.assertRaises(ValueError):
            capacity_environment(self.variants(), 1024, "timing", "negative-on")

    def test_negative_setting_needs_explicit_zero_or_one_from_each_mounted_core(self):
        for enabled in (False, True):
            value = profiles(256)
            for instance, scope in enumerate(("rootfs", "pvisor-workspace"), 10):
                value["rows"].append(
                    dict(pid=100, component="overlay-core", instance=instance, scope=scope)
                )
                value["final_profiles"].append(
                    dict(
                        pid=100,
                        component="overlay-core",
                        instance=instance,
                        final_record=True,
                        measurements={NEGATIVE_COUNTER: {"units": int(enabled)}},
                    )
                )
            self.assertEqual(len(validate_profile_negative_cache(value, enabled)), 2)
            for fault in ("missing_counter", "wrong_setting", "unfinished"):
                changed = copy.deepcopy(value)
                profile = changed["final_profiles"][2]
                if fault == "missing_counter":
                    profile["measurements"].clear()
                elif fault == "wrong_setting":
                    profile["measurements"][NEGATIVE_COUNTER]["units"] = int(not enabled)
                else:
                    profile["final_record"] = False
                with self.subTest(enabled=enabled, fault=fault), self.assertRaises(ValueError):
                    validate_profile_negative_cache(changed, enabled)

    def test_upper_treatment_is_independent_and_requires_actual_per_owner_setting(self):
        config = self.variants()
        for variant in config["variants"]:
            variant["env"] = {UPPER_ENV: variant["env"][NEGATIVE_ENV]}
        self.assertEqual(capacity_conditions(config), [("negative-off", 256), ("negative-on", 256)])
        environment = capacity_environment(config, 256, "profiles", "negative-on")
        self.assertEqual(environment[UPPER_ENV], "1")
        self.assertEqual(environment[NEGATIVE_ENV], "0")
        mixed = copy.deepcopy(config)
        mixed["variants"][0]["env"] = {NEGATIVE_ENV: "0"}
        with self.assertRaises(ValueError):
            capacity_conditions(mixed)
        for enabled in (False, True):
            value = profiles(256)
            for instance, scope in enumerate(("rootfs", "pvisor-workspace"), 10):
                value["rows"].append(
                    dict(pid=100, component="overlay-core", instance=instance, scope=scope)
                )
                value["final_profiles"].append(
                    dict(
                        pid=100,
                        component="overlay-core",
                        instance=instance,
                        final_record=True,
                        measurements={UPPER_COUNTER: {"units": int(enabled)}},
                    )
                )
            self.assertEqual(len(validate_profile_upper_absence_cache(value, enabled)), 2)
            value["final_profiles"][2]["measurements"].pop(UPPER_COUNTER)
            with self.assertRaises(ValueError):
                validate_profile_upper_absence_cache(value, enabled)

    def test_readonly_flush_treatment_keeps_other_optimizations_and_guest_controls_fixed(self):
        config = self.variants()
        for variant in config["variants"]:
            variant["env"] = {FLUSH_ENV: variant["env"][NEGATIVE_ENV]}
        self.assertEqual(capacity_conditions(config), [("negative-off", 256), ("negative-on", 256)])
        for mode in ("timing", "profiles"):
            for condition, expected in (("negative-off", "0"), ("negative-on", "1")):
                env = capacity_environment(config, 256, mode, condition)
                self.assertEqual(env[FLUSH_ENV], expected)
                self.assertEqual((env[NEGATIVE_ENV], env[UPPER_ENV]), ("0", "0"))
                self.assertEqual((env[CAPACITY_ENV], env["PVISOR_VM_FS_WRITEBACK"]), ("256", "0"))
                self.assertEqual(env["PVISOR_FS_PROFILE"], "1" if mode == "profiles" else "0")
        for environment in (
            {FLUSH_ENV: "2"},
            {FLUSH_ENV: 1},
            {FLUSH_ENV: "1", UPPER_ENV: "0"},
            {FLUSH_ENV: "1", "PVISOR_VM_FS_WRITEBACK": "0"},
        ):
            invalid = copy.deepcopy(config)
            invalid["variants"][1]["env"] = environment
            with self.subTest(environment=environment), self.assertRaises(ValueError):
                capacity_conditions(invalid)
        invalid = copy.deepcopy(config)
        invalid["variants"][0]["env"] = {UPPER_ENV: "0"}
        with self.assertRaises(ValueError):
            capacity_conditions(invalid)

    def test_readonly_flush_verification_requires_explicit_actual_zero_or_one_per_overlay(self):
        for enabled in (False, True):
            value = profiles(256)
            for final in value["final_profiles"]:
                final["measurements"][FLUSH_COUNTER] = {"units": int(enabled)}
            self.assertEqual(len(validate_profile_readonly_flush_elision(value, enabled)), 2)
            # An unrelated enabled owner cannot fill in a missing mounted setting.
            value["final_profiles"].append(
                dict(
                    pid=100,
                    component="virtio-fs-overlay",
                    instance=99,
                    final_record=True,
                    measurements={FLUSH_COUNTER: {"units": int(enabled)}},
                )
            )
            for fault in ("missing", "wrong", "bool", "unfinished", "missing_scope"):
                changed = copy.deepcopy(value)
                final = changed["final_profiles"][0]
                if fault == "missing":
                    final["measurements"].pop(FLUSH_COUNTER)
                elif fault == "wrong":
                    final["measurements"][FLUSH_COUNTER]["units"] = int(not enabled)
                elif fault == "bool":
                    final["measurements"][FLUSH_COUNTER]["units"] = enabled
                elif fault == "unfinished":
                    final["final_record"] = False
                else:
                    changed["rows"].pop()
                with self.subTest(enabled=enabled, fault=fault), self.assertRaises(ValueError):
                    validate_profile_readonly_flush_elision(changed, enabled)
        with self.assertRaises(ValueError):
            validate_profile_readonly_flush_elision(profiles(256), 0)

    def test_lower_data_treatment_cannot_also_enable_flush_or_absence_caches(self):
        config = self.variants()
        for variant in config["variants"]:
            variant["env"] = {DATA_ENV: variant["env"][NEGATIVE_ENV]}
        for mode in ("timing", "profiles"):
            for arm, expected in (("negative-off", "0"), ("negative-on", "1")):
                env = capacity_environment(config, 256, mode, arm)
                self.assertEqual(env[DATA_ENV], expected)
                self.assertEqual(
                    (env[NEGATIVE_ENV], env[UPPER_ENV], env[FLUSH_ENV]), ("0", "0", "0")
                )
                self.assertEqual(env["PVISOR_VM_FS_WRITEBACK"], "0")
                self.assertEqual(env[CAPACITY_ENV], "256")
        for treatment in ({DATA_ENV: "2"}, {DATA_ENV: 1}, {DATA_ENV: "1", FLUSH_ENV: "0"}):
            invalid = copy.deepcopy(config)
            invalid["variants"][1]["env"] = treatment
            with self.subTest(treatment=treatment), self.assertRaises(ValueError):
                capacity_conditions(invalid)

    def test_lower_data_requires_each_mounted_actual_setting_and_never_missing_as_zero(self):
        for enabled in (False, True):
            valid = profiles(256)
            for final in valid["final_profiles"]:
                final["measurements"][DATA_COUNTER] = {"units": int(enabled)}
            self.assertEqual(len(validate_profile_lower_data_cache(valid, enabled)), 2)
            for fault in ("missing", "wrong", "unfinished", "scope", "bool"):
                changed = copy.deepcopy(valid)
                final = changed["final_profiles"][0]
                if fault == "missing":
                    final["measurements"].pop(DATA_COUNTER)
                elif fault == "wrong":
                    final["measurements"][DATA_COUNTER]["units"] = int(not enabled)
                elif fault == "unfinished":
                    final["final_record"] = False
                elif fault == "scope":
                    changed["rows"].pop()
                else:
                    final["measurements"][DATA_COUNTER]["units"] = enabled
                with self.subTest(enabled=enabled, fault=fault), self.assertRaises(ValueError):
                    validate_profile_lower_data_cache(changed, enabled)

    def test_explicit_factorial_design_covers_four_combinations_with_fixed_controls(self):
        config = factorial_config()
        self.assertEqual(
            capacity_conditions(config),
            [("neither", 256), ("data", 256), ("flush", 256), ("both", 256)],
        )
        for mode in ("timing", "profiles"):
            for variant in config["variants"]:
                with self.subTest(mode=mode, condition=variant["name"]):
                    env = capacity_environment(config, 256, mode, variant["name"])
                    self.assertEqual(
                        {key: env[key] for key in (DATA_ENV, FLUSH_ENV)}, variant["env"]
                    )
                    self.assertEqual((env[NEGATIVE_ENV], env[UPPER_ENV]), ("0", "0"))
                    self.assertEqual(
                        (env[CAPACITY_ENV], env["PVISOR_VM_FS_WRITEBACK"]), ("256", "0")
                    )
                    self.assertEqual(env["PVISOR_VM_GUEST_CACHE"], "1")
                    self.assertEqual(env["PVISOR_FS_OWNED_INPUTS_CONDITION"], "both")
                    self.assertEqual(env["PVISOR_FS_PROFILE"], "1" if mode == "profiles" else "0")

    def test_factorial_requires_explicit_complete_unique_combinations_and_no_other_treatments(self):
        invalids = []
        missing_design = factorial_config()
        missing_design.pop("treatment_design")
        invalids.append(missing_design)
        for design in (None, "lower-data-local-flush", ""):
            invalids.append(dict(factorial_config(), treatment_design=design))
        missing_variants = factorial_config()
        missing_variants.pop("variants")
        invalids.append(missing_variants)
        missing_arm = factorial_config()
        missing_arm["variants"].pop()
        invalids.append(missing_arm)
        duplicate = factorial_config()
        duplicate["variants"][-1]["env"] = duplicate["variants"][0]["env"].copy()
        invalids.append(duplicate)
        for capacity in (1024, True, "256"):
            changed = factorial_config()
            changed["variants"][0]["capacity"] = capacity
            invalids.append(changed)
        for env in (
            {DATA_ENV: "0"},
            {FLUSH_ENV: "0"},
            {DATA_ENV: 0, FLUSH_ENV: "0"},
            {DATA_ENV: "0", FLUSH_ENV: True},
            {DATA_ENV: "0", FLUSH_ENV: "2"},
            *(
                {DATA_ENV: "0", FLUSH_ENV: "0", key: "0"}
                for key in (NEGATIVE_ENV, UPPER_ENV, "PVISOR_VM_FS_WRITEBACK", "PVISOR_FS_PROFILE")
            ),
        ):
            changed = factorial_config()
            changed["variants"][0]["env"] = env
            invalids.append(changed)
        for config in invalids:
            with self.subTest(config=config), self.assertRaises(ValueError):
                capacity_conditions(config)

    def test_factorial_diagnostics_require_both_actual_factors_and_all_fixed_owner_settings(self):
        config = factorial_config()
        for variant in config["variants"]:
            env = capacity_environment(config, 256, "profiles", variant["name"])
            actual = factorial_profiles(env)
            evidence = validate_profile_factorial_settings(actual, env)
            for field in (
                "capacity_verification",
                "negative_cache_verification",
                "upper_absence_cache_verification",
                "readonly_flush_elision_verification",
                "lower_data_cache_verification",
            ):
                self.assertEqual(set(evidence[field]), {"rootfs", "pvisor-workspace"})
            for index, counter in (
                (0, DATA_COUNTER),
                (1, FLUSH_COUNTER),
                (0, "directory_cache_capacity"),
                (2, NEGATIVE_COUNTER),
                (3, UPPER_COUNTER),
            ):
                for fault in ("missing", "wrong", "bool", "unfinished"):
                    changed = copy.deepcopy(actual)
                    final = changed["final_profiles"][index]
                    if fault == "missing":
                        final["measurements"].pop(counter)
                    elif fault == "wrong":
                        final["measurements"][counter]["units"] += 1
                    elif fault == "bool":
                        final["measurements"][counter]["units"] = bool(
                            final["measurements"][counter]["units"]
                        )
                    else:
                        final["final_record"] = False
                    with (
                        self.subTest(arm=variant["name"], counter=counter, fault=fault),
                        self.assertRaises(ValueError),
                    ):
                        validate_profile_factorial_settings(changed, env)
            for component in ("virtio-fs-overlay", "overlay-core"):
                changed = copy.deepcopy(actual)
                changed["rows"] = [
                    row
                    for row in changed["rows"]
                    if (row["component"], row["scope"]) != (component, "pvisor-workspace")
                ]
                with (
                    self.subTest(arm=variant["name"], missing_component=component),
                    self.assertRaises(ValueError),
                ):
                    validate_profile_factorial_settings(changed, env)

    def test_factorial_expected_settings_cannot_be_missing_or_silently_reset(self):
        config = factorial_config()
        env = capacity_environment(config, 256, "profiles", "both")
        actual = factorial_profiles(env)
        for key in (
            DATA_ENV,
            FLUSH_ENV,
            CAPACITY_ENV,
            NEGATIVE_ENV,
            UPPER_ENV,
            NEGATIVE_ENTRY_ENV,
            "PVISOR_VM_FS_WRITEBACK",
            "PVISOR_VM_GUEST_CACHE",
            "PVISOR_FS_PROFILE",
            "PVISOR_FS_OWNED_INPUTS_CONDITION",
        ):
            for fault in ("missing", "changed"):
                changed = env.copy()
                if fault == "missing":
                    changed.pop(key)
                else:
                    changed[key] = "0" if env[key] == "1" else "1"
                with self.subTest(key=key, fault=fault), self.assertRaises(ValueError):
                    validate_profile_factorial_settings(actual, changed)

    def test_negative_entry_design_varies_only_its_switch_over_enabled_data_and_flush(self):
        config = negative_entry_config()
        self.assertEqual(capacity_conditions(config), [("entry-off", 256), ("entry-on", 256)])
        for mode in ("timing", "profiles"):
            for variant in config["variants"]:
                env = capacity_environment(config, 256, mode, variant["name"])
                self.assertEqual(env[NEGATIVE_ENTRY_ENV], variant["env"][NEGATIVE_ENTRY_ENV])
                self.assertEqual((env[DATA_ENV], env[FLUSH_ENV]), ("1", "1"))
                self.assertEqual((env[NEGATIVE_ENV], env[UPPER_ENV]), ("0", "0"))
                self.assertEqual((env[CAPACITY_ENV], env["PVISOR_VM_FS_WRITEBACK"]), ("256", "0"))
                self.assertEqual(env["PVISOR_VM_GUEST_CACHE"], "1")
                self.assertEqual(env["PVISOR_FS_OWNED_INPUTS_CONDITION"], "both")
                self.assertEqual(env["PVISOR_FS_PROFILE"], "1" if mode == "profiles" else "0")
        self.assertEqual(
            capacity_environment(self.variants(), 256, "timing", "negative-on")[NEGATIVE_ENTRY_ENV],
            "0",
        )
        self.assertEqual(
            capacity_environment(factorial_config(), 256, "timing", "both")[NEGATIVE_ENTRY_ENV], "0"
        )

    def test_negative_entry_design_requires_exact_off_on_arms_and_explicit_discriminator(self):
        invalids = []
        for design in (None, FACTORIAL_DESIGN, "negative-entry"):
            invalids.append(dict(negative_entry_config(), treatment_design=design))
        legacy = negative_entry_config()
        legacy.pop("treatment_design")
        invalids.append(legacy)
        missing = negative_entry_config()
        missing.pop("variants")
        invalids.append(missing)
        for count in (1, 3):
            changed = negative_entry_config()
            changed["variants"] = (
                changed["variants"][:count]
                if count == 1
                else changed["variants"]
                + [dict(name="third", capacity=256, env={NEGATIVE_ENTRY_ENV: "1"})]
            )
            invalids.append(changed)
        duplicate = negative_entry_config()
        duplicate["variants"][1]["env"][NEGATIVE_ENTRY_ENV] = "0"
        invalids.append(duplicate)
        for env in (
            {},
            {NEGATIVE_ENTRY_ENV: 1},
            {NEGATIVE_ENTRY_ENV: True},
            {NEGATIVE_ENTRY_ENV: "2"},
            {DATA_ENV: "1"},
            *(
                {NEGATIVE_ENTRY_ENV: "1", key: "0"}
                for key in (
                    DATA_ENV,
                    FLUSH_ENV,
                    NEGATIVE_ENV,
                    UPPER_ENV,
                    "PVISOR_FS_PROFILE",
                    "PVISOR_VM_FS_WRITEBACK",
                )
            ),
        ):
            changed = negative_entry_config()
            changed["variants"][1]["env"] = env
            invalids.append(changed)
        for capacity in (1024, True, "256"):
            changed = negative_entry_config()
            changed["variants"][0]["capacity"] = capacity
            invalids.append(changed)
        mixed_factorial = factorial_config()
        mixed_factorial["variants"][0]["env"][NEGATIVE_ENTRY_ENV] = "0"
        invalids.append(mixed_factorial)
        for config in invalids:
            with self.subTest(config=config), self.assertRaises(ValueError):
                capacity_conditions(config)

    def test_negative_entry_diagnostics_require_actual_eligibility_and_explicit_reply_counts(self):
        config = negative_entry_config()
        for arm in ("entry-off", "entry-on"):
            env = capacity_environment(config, 256, "profiles", arm)
            valid = negative_entry_profiles(env)
            evidence = validate_profile_negative_entry_settings(valid, env)
            self.assertEqual(
                set(evidence["negative_entry_cache_verification"]), {"rootfs", "pvisor-workspace"}
            )
            self.assertTrue(
                all(
                    actual["replies"] == 0
                    for actual in evidence["negative_entry_cache_verification"].values()
                )
            )
            for index in (0, 1):
                for counter in (NEGATIVE_ENTRY_COUNTER, NEGATIVE_ENTRY_REPLIES):
                    for fault in ("missing", "bool", "negative", "wrong_setting", "unfinished"):
                        changed = copy.deepcopy(valid)
                        final = changed["final_profiles"][index]
                        if fault == "missing":
                            final["measurements"].pop(counter)
                        elif fault == "unfinished":
                            final["final_record"] = False
                        else:
                            if fault == "bool":
                                units = False
                            elif fault == "negative":
                                units = -1
                            elif counter == NEGATIVE_ENTRY_COUNTER:
                                units = 1 - int(env[NEGATIVE_ENTRY_ENV])
                            else:
                                units = 0.5
                            final["measurements"][counter]["units"] = units
                        with (
                            self.subTest(arm=arm, index=index, counter=counter, fault=fault),
                            self.assertRaises(ValueError),
                        ):
                            validate_profile_negative_entry_settings(changed, env)
            changed = copy.deepcopy(valid)
            changed["final_profiles"][0]["measurements"][NEGATIVE_ENTRY_REPLIES]["units"] = 17
            if arm == "entry-off":
                with self.assertRaises(ValueError):
                    validate_profile_negative_entry_settings(changed, env)
            else:
                actual = validate_profile_negative_entry_settings(changed, env)[
                    "negative_entry_cache_verification"
                ]
                self.assertEqual(
                    (actual["rootfs"]["replies"], actual["pvisor-workspace"]["replies"]), (17, 0)
                )

    def test_negative_entry_diagnostics_reject_changed_or_missing_fixed_settings(self):
        env = capacity_environment(negative_entry_config(), 256, "profiles", "entry-on")
        valid = negative_entry_profiles(env)
        for key in (
            DATA_ENV,
            FLUSH_ENV,
            CAPACITY_ENV,
            NEGATIVE_ENV,
            UPPER_ENV,
            NEGATIVE_ENTRY_ENV,
            "PVISOR_FS_PROFILE",
            "PVISOR_VM_FS_WRITEBACK",
            "PVISOR_VM_GUEST_CACHE",
        ):
            changed = env.copy()
            changed.pop(key)
            with self.subTest(missing=key), self.assertRaises(ValueError):
                validate_profile_negative_entry_settings(valid, changed)
        for index, counter in (
            (0, DATA_COUNTER),
            (1, FLUSH_COUNTER),
            (0, "directory_cache_capacity"),
            (2, NEGATIVE_COUNTER),
            (3, UPPER_COUNTER),
        ):
            for fault in ("missing", "changed"):
                changed = copy.deepcopy(valid)
                measurements = changed["final_profiles"][index]["measurements"]
                if fault == "missing":
                    measurements.pop(counter)
                else:
                    measurements[counter]["units"] = 0 if measurements[counter]["units"] else 1
                with self.subTest(counter=counter, fault=fault), self.assertRaises(ValueError):
                    validate_profile_negative_entry_settings(changed, env)


class SharedOpenContractTests(unittest.TestCase):
    def config(self):
        return dict(
            owned_inputs_manifest="/sealed/inputs.json",
            treatment_design=SHARED_OPEN_DESIGN,
            variants=[
                dict(name="off", capacity=256, env={SHARED_OPEN_ENV: "0"}),
                dict(name="on", capacity=256, env={SHARED_OPEN_ENV: "1"}),
            ],
        )

    def evidence(self, environment):
        value = negative_entry_profiles(environment)
        for final in value["final_profiles"][:2]:
            final["measurements"][SHARED_OPEN_COUNTER] = {
                "units": int(environment[SHARED_OPEN_ENV])
            }
            final["measurements"].update({name: {"units": 0} for name in QUALIFICATION_COUNTERS})
        return value

    def test_only_explicit_binary_shared_switch_can_vary(self):
        for mode in ("timing", "profiles"):
            off, on = [capacity_environment(self.config(), 256, mode, arm) for arm in ("off", "on")]
            self.assertEqual({k for k in off if off[k] != on[k]}, {SHARED_OPEN_ENV})
            self.assertEqual((on[DATA_ENV], on[FLUSH_ENV], on[NEGATIVE_ENTRY_ENV]), ("1", "1", "0"))
            self.assertEqual(on["RUST_LOG"], "off")
        for environment in (
            {SHARED_OPEN_ENV: "2"},
            {SHARED_OPEN_ENV: 1},
            {SHARED_OPEN_ENV: "1", DATA_ENV: "1"},
            {FLUSH_ENV: "1"},
        ):
            config = self.config()
            config["variants"][1]["env"] = environment
            with self.subTest(environment=environment), self.assertRaises(ValueError):
                capacity_conditions(config)
        for fault in ("same", "capacity", "extra_arm"):
            config = self.config()
            if fault == "same":
                config["variants"][1]["env"][SHARED_OPEN_ENV] = "0"
            elif fault == "capacity":
                config["variants"][1]["capacity"] = 1024
            else:
                config["variants"].append(
                    dict(name="third", capacity=256, env={SHARED_OPEN_ENV: "0"})
                )
            with self.subTest(fault=fault), self.assertRaises(ValueError):
                capacity_conditions(config)

    def test_explicit_zero_query_labels_are_evidence_not_missing_defaults(self):
        for arm in ("off", "on"):
            env = capacity_environment(self.config(), 256, "profiles", arm)
            value = self.evidence(env)
            validate_profile_shared_open_settings(value, env)
            for label in (SHARED_OPEN_COUNTER, *QUALIFICATION_COUNTERS):
                changed = copy.deepcopy(value)
                changed["final_profiles"][1]["measurements"].pop(label)
                with self.subTest(arm=arm, missing=label), self.assertRaises(ValueError):
                    validate_profile_shared_open_settings(changed, env)
            changed = copy.deepcopy(value)
            changed["rows"][0]["raw_measurements"] = {
                name: {"units": 0} for name in QUALIFICATION_COUNTERS
            }
            changed["rows"][0]["raw_measurements"].pop(QUALIFICATION_COUNTERS[0])
            with self.assertRaises(ValueError):
                validate_profile_shared_open_settings(changed, env)
            for key in (
                DATA_ENV,
                FLUSH_ENV,
                NEGATIVE_ENTRY_ENV,
                NEGATIVE_ENV,
                UPPER_ENV,
                CAPACITY_ENV,
                SHARED_OPEN_ENV,
                "RUST_LOG",
                "PVISOR_FS_PROFILE",
            ):
                changed = env.copy()
                changed[key] = "1" if env[key] == "0" else "0"
                if key == SHARED_OPEN_ENV:
                    changed[key] = "2"
                with self.subTest(pollution=key), self.assertRaises(ValueError):
                    validate_profile_shared_open_settings(value, changed)

    def test_entire_unmounted_owner_loss_and_duplicate_final_are_rejected(self):
        # Independent fixture for the owned driver lifecycle: these include
        # unmounted setup/reader instances, which phase-only checks cannot see.
        components = (
            ["overlay-core"] * 4
            + ["preimage-log"] * 3
            + ["preimage-log-read"] * 4
            + ["virtio-fs-overlay"] * 2
            + ["virtio-fs-protocol"] * 2
            + ["virtio-fs-dispatch"] * 2
        )
        finals = [
            dict(pid=10, component=name, instance=i, final_record=True, measurements={})
            for i, name in enumerate(components)
        ]

        def lines(records):
            return [
                prefix + json.dumps(record)
                for record in records
                for prefix in ("pvisor-fs-profile-start ", "pvisor-fs-profile ")
            ]

        self.assertEqual(
            validate_shared_open_inventory({"final_profiles": finals}, lines(finals))[
                "final_instances"
            ],
            17,
        )
        for fault in ("whole_owner", "duplicate_final"):
            altered = finals[:-1] if fault == "whole_owner" else finals
            raw = lines(altered)
            if fault == "duplicate_final":
                raw.append("pvisor-fs-profile " + json.dumps(finals[0]))
            with self.subTest(fault=fault), self.assertRaises(ValueError):
                validate_shared_open_inventory({"final_profiles": altered}, raw)


if __name__ == "__main__":
    unittest.main()

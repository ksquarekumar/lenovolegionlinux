// SPDX-License-Identifier: GPL-2.0
/*
 * Lenovo Legion WMI driver for the Legion Pro 7 16IRX8H (82WQ).
 *
 * One driver for the whole Legion WMI of this laptop, in place of
 * lenovo-wmi-gamezone, lenovo-wmi-other, lenovo-wmi-capdata,
 * lenovo-wmi-events, lenovo-wmi-hotkey-utilities and the fan calls through
 * acpi_call:
 *
 * - platform_profile from the GameZone thermal modes, with the Fn+Q event;
 * - the firmware-attributes class device lenovo-wmi-other-0 (CPU and GPU
 *   limits, with the bounds from Capability Data 01);
 * - a hwmon device: fan RPM, the custom-mode fan table (one table for both
 *   fans, levels 1-10), full speed (pwm1_enable 0) and fan max mode;
 * - the micmute and mute LEDs of the Lenovo Utility interface.
 *
 * The GameZone device is the hub: it binds after the Other Mode, Capability
 * Data 01 and fan method devices, and device links unbind it before them.
 *
 * Sources (cited at each definition):
 * [UP] Linux drivers/platform/x86/lenovo/wmi-{gamezone,other,capdata,events,
 *      hotkey-utilities,helpers}.c (Derek J. Clark, Rong Zhang, Lenovo).
 * [LLL] LenovoLegionLinux kernel_module/legion-laptop.c, model_kwcn.
 * [DOC] legionctl docs/design/02-platform.md (Fan control) and 10-gap.md: calls
 *      measured on this machine on 2026-10-12, and the fan control of LLL
 *      model_kwcn at HEAD 6cf67c3.
 */

#include <linux/acpi.h>
#include <linux/bits.h>
#include <linux/cleanup.h>
#include <linux/device.h>
#include <linux/hwmon.h>
#include <linux/hwmon-sysfs.h>
#include <linux/kobject.h>
#include <linux/leds.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/platform_profile.h>
#include <linux/pm.h>
#include <linux/slab.h>
#include <linux/sysfs.h>
#include <linux/types.h>
#include <linux/unaligned.h>
#include <linux/wmi.h>
#include <linux/workqueue.h>

#include "firmware_attributes_class.h"

/* [UP] wmi-gamezone.c LENOVO_GAMEZONE_GUID; object AA, \_SB.GZFD.WMAA here */
#define LWMI_GAMEZONE_GUID	"887B54E3-DDDC-4B2C-8B88-68A26A8835D0"
/* [UP] wmi-other.c LENOVO_OTHER_MODE_GUID; object AE, \_SB.GZFD.WMAE here */
#define LWMI_OTHER_GUID		"DC2A8805-3A8C-41BA-A6F7-092E0089CD3B"
/* [UP] wmi-capdata.c LENOVO_CAPABILITY_DATA_01_GUID; object A5, WQA5 */
#define LWMI_CAPDATA01_GUID	"7A8F5407-CB67-4D6E-B547-39B3BE018154"
/* [LLL] WMI_GUID_LENOVO_FAN_METHOD; object AB, \_SB.GZFD.WMAB here */
#define LWMI_FAN_GUID		"92549549-4BDE-4F06-AC04-CE8BF898DBAA"
/* [UP] wmi-events.c THERMAL_MODE_EVENT_GUID; notify id 0xE7 here */
#define LWMI_EVENT_GUID		"D320289E-8FEA-41E0-86F9-911D83151B5F"
/* [UP] wmi-hotkey-utilities.c LUD_WMI_METHOD_GUID; object SK here */
#define LWMI_UTILITY_GUID	"CE6C0974-0407-4F50-88BA-4FC3B6559AD8"

/* [UP] wmi-gamezone.c LWMI_GZ_METHOD_ID_SMARTFAN_* */
#define LWMI_GZ_SMARTFAN_SUP	43
#define LWMI_GZ_SMARTFAN_SET	44
#define LWMI_GZ_SMARTFAN_GET	45
/* [DOC] GameZone 0x0D: fan max mode, one byte (1 on, 0 off), no read */
#define LWMI_GZ_FAN_MAX		0x0D

/* [UP] wmi-helpers.h enum thermal_mode */
#define LWMI_MODE_NONE		0x00
#define LWMI_MODE_QUIET		0x01
#define LWMI_MODE_BALANCED	0x02
#define LWMI_MODE_PERFORMANCE	0x03
#define LWMI_MODE_EXTREME	0xE0
#define LWMI_MODE_CUSTOM	0xFF

/* [UP] wmi-other.c LWMI_FEATURE_VALUE_GET/SET */
#define LWMI_OM_GET		17
#define LWMI_OM_SET		18

/* [UP] wmi-capdata.h lwmi_attr_id(): device, feature, mode, type */
#define LWMI_DEV_CPU		0x01
#define LWMI_DEV_GPU		0x02
#define LWMI_DEV_FAN		0x04
#define LWMI_ATTR_ID(dev, feat, mode, type) \
	((u32)(dev) << 24 | (u32)(feat) << 16 | (u32)(mode) << 8 | (u32)(type))
/* [UP] wmi-other.c LWMI_ATTR_ID_FAN_RPM: 0x04030001, 0x04030002 */
#define LWMI_FAN_RPM(fan)	LWMI_ATTR_ID(LWMI_DEV_FAN, 0x03, 0, (fan) + 1)
/* [DOC] Other Method feature 0x04020000: full speed, custom mode only */
#define LWMI_FAN_FULL_SPEED	LWMI_ATTR_ID(LWMI_DEV_FAN, 0x02, 0, 0)

/* [LLL] WMI_METHOD_ID_FAN_GET_TABLE, WMI_METHOD_ID_FAN_SET_TABLE */
#define LWMI_FAN_GET_TABLE	5
#define LWMI_FAN_SET_TABLE	6
#define LWMI_FAN_POINTS		10
/* [LLL] the DSDT reads FSS0-FSS9 with CreateByteField at 0x06 + 2 * i */
#define LWMI_FAN_SET_SIZE	0x40
#define LWMI_FAN_SET_OFFSET(i)	(0x06 + 2 * (i))
#define LWMI_FAN_LEVEL_MAX	10

/* [UP] wmi-hotkey-utilities.c */
#define LWMI_LUD_GET_SUPPORT		1
#define LWMI_LUD_SET_FEATURE		2
#define LWMI_LUD_MICMUTE_VER		20
#define LWMI_LUD_AUDIOMUTE_VER		26
#define LWMI_LUD_MICMUTE_SUPPORTED	25
#define LWMI_LUD_AUDIOMUTE_SUPPORTED	27
#define LWMI_LUD_MICMUTE_ON		1
#define LWMI_LUD_MICMUTE_OFF		2
#define LWMI_LUD_AUDIOMUTE_ON		4
#define LWMI_LUD_AUDIOMUTE_OFF		5

/* [DOC] the lowest level the firmware accepts at each point of the fan table */
static const u8 lwmi_fan_min[LWMI_FAN_POINTS] = { 1, 1, 1, 1, 1, 1, 1, 1, 3, 5 };

enum lwmi_part {
	LWMI_OTHER,
	LWMI_CAPDATA01,
	LWMI_FAN,
	LWMI_EVENT,
	LWMI_UTILITY,
	LWMI_PARTS,
};

struct lwmi_priv {
	struct wmi_device *gz;
	struct wmi_device *other;
	struct wmi_device *cd01;
	struct wmi_device *fan;
	struct device *ppdev;
	struct device *fw_dev;
	struct kset *fw_kset;
	struct work_struct event_work;
	struct mutex lock; /* WMI call sequences and the state below */
	u8 curve[LWMI_FAN_POINTS];
	bool curve_set;
	bool fan_max;
	bool full_speed;
};

static DEFINE_MUTEX(lwmi_registry_lock);
static struct wmi_device *lwmi_parts[LWMI_PARTS];
static struct lwmi_priv *lwmi_hub;

static int lwmi_call(struct wmi_device *wdev, u32 method, const void *in,
		     size_t in_len, void *out, size_t out_len)
{
	struct wmi_buffer win = { .length = in_len, .data = (void *)in };
	struct wmi_buffer wout;
	int ret;

	if (!out)
		return wmidev_invoke_procedure(wdev, 0, method, &win);

	ret = wmidev_invoke_method(wdev, 0, method, &win, &wout, out_len);
	if (ret)
		return ret;

	memcpy(out, wout.data, out_len);
	kfree(wout.data);
	return 0;
}

static int lwmi_call_u32(struct wmi_device *wdev, u32 method, const void *in,
			 size_t in_len, u32 *value)
{
	__le32 out;
	int ret;

	ret = lwmi_call(wdev, method, in, in_len, &out, sizeof(out));
	if (!ret)
		*value = le32_to_cpu(out);
	return ret;
}

/* Other Mode GetFeatureValue/SetFeatureValue: the attribute id, then the value */
static int lwmi_feature_get(struct lwmi_priv *priv, u32 id, u32 *value)
{
	__le32 args[2] = { cpu_to_le32(id) };

	return lwmi_call_u32(priv->other, LWMI_OM_GET, args, sizeof(args), value);
}

/* [UP] wmi-other.c attr_current_value_store(): the reply carries nothing to check */
static int lwmi_feature_set(struct lwmi_priv *priv, u32 id, u32 value)
{
	__le32 args[2] = { cpu_to_le32(id), cpu_to_le32(value) };

	return lwmi_call(priv->other, LWMI_OM_SET, args, sizeof(args), NULL, 0);
}

static int lwmi_mode(struct lwmi_priv *priv, u32 *mode)
{
	return lwmi_call_u32(priv->gz, LWMI_GZ_SMARTFAN_GET, NULL, 0, mode);
}

static const char *lwmi_mode_name(u32 mode)
{
	switch (mode) {
	case LWMI_MODE_QUIET:
		return "low-power";
	case LWMI_MODE_BALANCED:
		return "balanced";
	case LWMI_MODE_PERFORMANCE:
		return "performance";
	case LWMI_MODE_EXTREME:
		return "max-power";
	case LWMI_MODE_CUSTOM:
		return "custom";
	default:
		return "unknown";
	}
}

/* -EBUSY with a warning that names the mode, unless the mode is custom */
static int lwmi_require_custom(struct lwmi_priv *priv, const char *what)
{
	u32 mode;
	int ret;

	ret = lwmi_mode(priv, &mode);
	if (ret)
		return ret;
	if (mode == LWMI_MODE_CUSTOM)
		return 0;

	dev_warn(&priv->gz->dev, "%s works in custom mode only; the mode is %s (%#x)\n",
		 what, lwmi_mode_name(mode), mode);
	return -EBUSY;
}

/* ======== Fan table, fan max mode, full speed ======== */

static int lwmi_fan_table_get(struct lwmi_priv *priv, u8 *levels)
{
	/* [LLL] WMIFanTableRead: u32 FSFL, then FSS0-FSS9 as u32 */
	struct {
		__le32 fsfl;
		__le32 fss[LWMI_FAN_POINTS];
	} __packed table;
	int ret, i;

	ret = lwmi_call(priv->fan, LWMI_FAN_GET_TABLE, NULL, 0, &table, sizeof(table));
	if (ret)
		return ret;

	for (i = 0; i < LWMI_FAN_POINTS; i++)
		levels[i] = le32_to_cpu(table.fss[i]);
	return 0;
}

static bool lwmi_fan_level_valid(int point, unsigned int level)
{
	return level >= lwmi_fan_min[point] && level <= LWMI_FAN_LEVEL_MAX;
}

static bool lwmi_fan_table_valid(const u8 *levels)
{
	int i;

	for (i = 0; i < LWMI_FAN_POINTS; i++) {
		if (!lwmi_fan_level_valid(i, levels[i]))
			return false;
		if (i && levels[i] < levels[i - 1])
			return false;
	}
	return true;
}

/* Write the table, then read it back: the firmware ignores it outside custom mode */
static int lwmi_fan_table_set(struct lwmi_priv *priv, const u8 *levels)
{
	u8 buf[LWMI_FAN_SET_SIZE] = {};
	u8 now[LWMI_FAN_POINTS];
	int ret, i;

	for (i = 0; i < LWMI_FAN_POINTS; i++)
		buf[LWMI_FAN_SET_OFFSET(i)] = levels[i];

	ret = lwmi_call(priv->fan, LWMI_FAN_SET_TABLE, buf, sizeof(buf), NULL, 0);
	if (ret)
		return ret;

	ret = lwmi_fan_table_get(priv, now);
	if (ret)
		return ret;

	if (memcmp(now, levels, sizeof(now))) {
		dev_err(&priv->gz->dev, "the fan table reads back as %*ph after writing %*ph\n",
			LWMI_FAN_POINTS, now, LWMI_FAN_POINTS, levels);
		return -EIO;
	}
	return 0;
}

static int lwmi_fan_max_set(struct lwmi_priv *priv, bool on)
{
	u8 arg = on;

	return lwmi_call(priv->gz, LWMI_GZ_FAN_MAX, &arg, sizeof(arg), NULL, 0);
}

static int lwmi_full_speed_set(struct lwmi_priv *priv, bool on)
{
	return lwmi_feature_set(priv, LWMI_FAN_FULL_SPEED, on);
}

/*
 * Set again what the firmware forgets: fan max mode after resume, and in
 * custom mode the fan table and full speed. Outside custom mode the firmware
 * resets the table and ends full speed.
 */
static void lwmi_reassert(struct lwmi_priv *priv, bool resume)
{
	u32 mode;
	int ret;

	guard(mutex)(&priv->lock);

	if (resume && priv->fan_max) {
		ret = lwmi_fan_max_set(priv, true);
		if (ret)
			dev_err(&priv->gz->dev, "cannot set fan max mode again: %d\n", ret);
	}

	ret = lwmi_mode(priv, &mode);
	if (ret)
		return;

	if (mode != LWMI_MODE_CUSTOM) {
		priv->full_speed = false;
		return;
	}

	if (priv->curve_set) {
		ret = lwmi_fan_table_set(priv, priv->curve);
		if (ret)
			dev_err(&priv->gz->dev, "cannot set the fan table again: %d\n", ret);
	}
	if (priv->full_speed) {
		ret = lwmi_full_speed_set(priv, true);
		if (ret)
			dev_err(&priv->gz->dev, "cannot set full speed again: %d\n", ret);
	}
}

static ssize_t pwm1_auto_point_pwm_show(struct device *dev, struct device_attribute *attr,
					char *buf)
{
	struct lwmi_priv *priv = dev_get_drvdata(dev);
	int point = to_sensor_dev_attr(attr)->index;
	u8 levels[LWMI_FAN_POINTS];
	int ret;

	scoped_guard(mutex, &priv->lock)
		ret = lwmi_fan_table_get(priv, levels);
	if (ret)
		return ret;

	return sysfs_emit(buf, "%u\n", levels[point]);
}

static ssize_t pwm1_auto_point_pwm_store(struct device *dev, struct device_attribute *attr,
					 const char *buf, size_t count)
{
	struct lwmi_priv *priv = dev_get_drvdata(dev);
	int point = to_sensor_dev_attr(attr)->index;
	u8 levels[LWMI_FAN_POINTS];
	unsigned int level;
	int ret;

	/* a level is 1-10: anything else, also a number out of range, is invalid */
	if (kstrtouint(buf, 10, &level) || !lwmi_fan_level_valid(point, level))
		return -EINVAL;

	guard(mutex)(&priv->lock);

	ret = lwmi_require_custom(priv, "the fan table");
	if (ret)
		return ret;

	ret = lwmi_fan_table_get(priv, levels);
	if (ret)
		return ret;

	levels[point] = level;
	if (!lwmi_fan_table_valid(levels))
		return -EINVAL;

	ret = lwmi_fan_table_set(priv, levels);
	if (ret)
		return ret;

	memcpy(priv->curve, levels, sizeof(priv->curve));
	priv->curve_set = true;
	return count;
}

static ssize_t fan_max_mode_show(struct device *dev, struct device_attribute *attr, char *buf)
{
	struct lwmi_priv *priv = dev_get_drvdata(dev);

	return sysfs_emit(buf, "%d\n", priv->fan_max);
}

static ssize_t fan_max_mode_store(struct device *dev, struct device_attribute *attr,
				  const char *buf, size_t count)
{
	struct lwmi_priv *priv = dev_get_drvdata(dev);
	bool on;
	int ret;

	ret = kstrtobool(buf, &on);
	if (ret)
		return ret;

	guard(mutex)(&priv->lock);

	ret = lwmi_fan_max_set(priv, on);
	if (ret)
		return ret;

	priv->fan_max = on;
	return count;
}

static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point1_pwm, pwm1_auto_point_pwm, 0);
static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point2_pwm, pwm1_auto_point_pwm, 1);
static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point3_pwm, pwm1_auto_point_pwm, 2);
static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point4_pwm, pwm1_auto_point_pwm, 3);
static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point5_pwm, pwm1_auto_point_pwm, 4);
static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point6_pwm, pwm1_auto_point_pwm, 5);
static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point7_pwm, pwm1_auto_point_pwm, 6);
static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point8_pwm, pwm1_auto_point_pwm, 7);
static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point9_pwm, pwm1_auto_point_pwm, 8);
static SENSOR_DEVICE_ATTR_RW(pwm1_auto_point10_pwm, pwm1_auto_point_pwm, 9);
static DEVICE_ATTR_RW(fan_max_mode);

static struct attribute *lwmi_fan_attrs[] = {
	&sensor_dev_attr_pwm1_auto_point1_pwm.dev_attr.attr,
	&sensor_dev_attr_pwm1_auto_point2_pwm.dev_attr.attr,
	&sensor_dev_attr_pwm1_auto_point3_pwm.dev_attr.attr,
	&sensor_dev_attr_pwm1_auto_point4_pwm.dev_attr.attr,
	&sensor_dev_attr_pwm1_auto_point5_pwm.dev_attr.attr,
	&sensor_dev_attr_pwm1_auto_point6_pwm.dev_attr.attr,
	&sensor_dev_attr_pwm1_auto_point7_pwm.dev_attr.attr,
	&sensor_dev_attr_pwm1_auto_point8_pwm.dev_attr.attr,
	&sensor_dev_attr_pwm1_auto_point9_pwm.dev_attr.attr,
	&sensor_dev_attr_pwm1_auto_point10_pwm.dev_attr.attr,
	&dev_attr_fan_max_mode.attr,
	NULL,
};
ATTRIBUTE_GROUPS(lwmi_fan);

static umode_t lwmi_hwmon_is_visible(const void *drvdata, enum hwmon_sensor_types type,
				     u32 attr, int channel)
{
	return type == hwmon_pwm ? 0644 : 0444;
}

static int lwmi_hwmon_read(struct device *dev, enum hwmon_sensor_types type, u32 attr,
			   int channel, long *val)
{
	struct lwmi_priv *priv = dev_get_drvdata(dev);
	u32 rpm;
	int ret;

	if (type == hwmon_pwm) {
		/* 0: no control, the fans at full speed; 2: the fan table */
		*val = priv->full_speed ? 0 : 2;
		return 0;
	}

	scoped_guard(mutex, &priv->lock)
		ret = lwmi_feature_get(priv, LWMI_FAN_RPM(channel), &rpm);
	if (ret)
		return ret;

	*val = rpm;
	return 0;
}

static int lwmi_hwmon_write(struct device *dev, enum hwmon_sensor_types type, u32 attr,
			    int channel, long val)
{
	struct lwmi_priv *priv = dev_get_drvdata(dev);
	int ret;

	if (val != 0 && val != 2)
		return -EINVAL;

	guard(mutex)(&priv->lock);

	ret = lwmi_require_custom(priv, "full speed");
	if (ret)
		return ret;

	ret = lwmi_full_speed_set(priv, !val);
	if (ret)
		return ret;

	priv->full_speed = !val;
	return 0;
}

static const struct hwmon_channel_info * const lwmi_hwmon_info[] = {
	HWMON_CHANNEL_INFO(fan, HWMON_F_INPUT, HWMON_F_INPUT),
	HWMON_CHANNEL_INFO(pwm, HWMON_PWM_ENABLE),
	NULL,
};

static const struct hwmon_ops lwmi_hwmon_ops = {
	.is_visible = lwmi_hwmon_is_visible,
	.read = lwmi_hwmon_read,
	.write = lwmi_hwmon_write,
};

static const struct hwmon_chip_info lwmi_hwmon_chip_info = {
	.ops = &lwmi_hwmon_ops,
	.info = lwmi_hwmon_info,
};

/* ======== Firmware attributes (Capability Data 01) ======== */

/* [UP] wmi-capdata.h struct capdata01 */
struct lwmi_capdata01 {
	__le32 id;
	__le32 supported;
	__le32 default_value;
	__le32 step;
	__le32 min_value;
	__le32 max_value;
};

struct lwmi_tunable {
	const char *name;
	u8 device_id;
	u8 feature_id;
	const char *display_name;
	u8 cd_mode; /* mode of its Capability Data 01 record */
	u8 cv_mode; /* mode of its current value */
	struct kobject *kobj;
};

/* [UP] wmi-other.c: the names, enum lwmi_feature_id_cpu/gpu and the display names */
static struct lwmi_tunable lwmi_tunables[] = {
	{ "cpu_temp", LWMI_DEV_CPU, 0x04, "Set the CPU thermal load limit" },
	{ "ppt_cpu_cl", LWMI_DEV_CPU, 0x06, "Set the CPU cross loading power limit" },
	{ "ppt_pl1_spl", LWMI_DEV_CPU, 0x02, "Set the CPU sustained power limit" },
	{ "ppt_pl1_tau", LWMI_DEV_CPU, 0x07, "Set the CPU sustained power limit exceed duration" },
	{ "ppt_pl2_sppt", LWMI_DEV_CPU, 0x01, "Set the CPU slow package power tracking limit" },
	{ "gpu_nv_ac_offset", LWMI_DEV_GPU, 0x04,
	  "Set the Nvidia GPU AC total processing power baseline offset" },
	{ "gpu_nv_ctgp", LWMI_DEV_GPU, 0x02, "Set the GPU configurable total graphics power" },
	{ "gpu_nv_ppab", LWMI_DEV_GPU, 0x01,
	  "Set the Nvidia GPU power performance aware boost limit" },
	{ "gpu_temp", LWMI_DEV_GPU, 0x03, "Set the GPU thermal load limit" },
};

static u32 lwmi_tunable_id(const struct lwmi_tunable *t, u8 mode)
{
	return LWMI_ATTR_ID(t->device_id, t->feature_id, mode, 0);
}

/* Each tunable is a kobject in attributes/, the kset of the class device */
static struct lwmi_tunable *lwmi_kobj_tunable(struct kobject *kobj)
{
	int i;

	for (i = 0; i < ARRAY_SIZE(lwmi_tunables); i++)
		if (lwmi_tunables[i].kobj == kobj)
			return &lwmi_tunables[i];
	return NULL;
}

static struct lwmi_priv *lwmi_kobj_priv(struct kobject *kobj)
{
	return dev_get_drvdata(kobj_to_dev(kobj->parent->parent));
}

/* Capability Data 01 has one record per instance; it changes with the power source */
static int lwmi_capdata01(struct lwmi_priv *priv, u32 id, struct lwmi_capdata01 *cd)
{
	u8 count = wmidev_instance_count(priv->cd01);
	struct wmi_buffer out;
	u8 i;

	for (i = 0; i < count; i++) {
		if (wmidev_query_block(priv->cd01, i, &out, sizeof(*cd)))
			continue;

		memcpy(cd, out.data, sizeof(*cd));
		kfree(out.data);
		if (le32_to_cpu(cd->id) == id)
			return 0;
	}
	return -ENODEV;
}

static ssize_t lwmi_capdata01_show(struct kobject *kobj, char *buf, size_t offset)
{
	struct lwmi_tunable *t = lwmi_kobj_tunable(kobj);
	struct lwmi_capdata01 cd;
	int ret;

	ret = lwmi_capdata01(lwmi_kobj_priv(kobj), lwmi_tunable_id(t, t->cd_mode), &cd);
	if (ret)
		return ret;

	return sysfs_emit(buf, "%u\n", get_unaligned_le32((u8 *)&cd + offset));
}

static ssize_t default_value_show(struct kobject *kobj, struct kobj_attribute *a, char *buf)
{
	return lwmi_capdata01_show(kobj, buf, offsetof(struct lwmi_capdata01, default_value));
}

static ssize_t max_value_show(struct kobject *kobj, struct kobj_attribute *a, char *buf)
{
	return lwmi_capdata01_show(kobj, buf, offsetof(struct lwmi_capdata01, max_value));
}

static ssize_t min_value_show(struct kobject *kobj, struct kobj_attribute *a, char *buf)
{
	return lwmi_capdata01_show(kobj, buf, offsetof(struct lwmi_capdata01, min_value));
}

static ssize_t scalar_increment_show(struct kobject *kobj, struct kobj_attribute *a, char *buf)
{
	return lwmi_capdata01_show(kobj, buf, offsetof(struct lwmi_capdata01, step));
}

static ssize_t display_name_show(struct kobject *kobj, struct kobj_attribute *a, char *buf)
{
	return sysfs_emit(buf, "%s\n", lwmi_kobj_tunable(kobj)->display_name);
}

static ssize_t type_show(struct kobject *kobj, struct kobj_attribute *a, char *buf)
{
	return sysfs_emit(buf, "integer\n");
}

static ssize_t current_value_show(struct kobject *kobj, struct kobj_attribute *a, char *buf)
{
	struct lwmi_tunable *t = lwmi_kobj_tunable(kobj);
	struct lwmi_priv *priv = lwmi_kobj_priv(kobj);
	u32 mode = LWMI_MODE_NONE;
	u32 value;
	int ret;

	guard(mutex)(&priv->lock);

	/* [UP] wmi-other.c: the value of the current mode, unless it has no modes */
	if (t->cv_mode != LWMI_MODE_NONE) {
		ret = lwmi_mode(priv, &mode);
		if (ret)
			return ret;
	}

	ret = lwmi_feature_get(priv, lwmi_tunable_id(t, mode), &value);
	if (ret)
		return ret;

	return sysfs_emit(buf, "%u\n", value);
}

static ssize_t current_value_store(struct kobject *kobj, struct kobj_attribute *a,
				   const char *buf, size_t count)
{
	struct lwmi_tunable *t = lwmi_kobj_tunable(kobj);
	struct lwmi_priv *priv = lwmi_kobj_priv(kobj);
	struct lwmi_capdata01 cd;
	u32 value;
	int ret;

	if (kstrtou32(buf, 10, &value))
		return -EINVAL;

	guard(mutex)(&priv->lock);

	ret = lwmi_capdata01(priv, lwmi_tunable_id(t, t->cd_mode), &cd);
	if (ret)
		return ret;
	if (value < le32_to_cpu(cd.min_value) || value > le32_to_cpu(cd.max_value))
		return -EINVAL;

	ret = lwmi_require_custom(priv, t->name);
	if (ret)
		return ret;

	ret = lwmi_feature_set(priv, lwmi_tunable_id(t, t->cv_mode), value);
	if (ret)
		return ret;

	return count;
}

static struct kobj_attribute lwmi_current_value = __ATTR_RW(current_value);
static struct kobj_attribute lwmi_default_value = __ATTR_RO(default_value);
static struct kobj_attribute lwmi_display_name = __ATTR_RO(display_name);
static struct kobj_attribute lwmi_max_value = __ATTR_RO(max_value);
static struct kobj_attribute lwmi_min_value = __ATTR_RO(min_value);
static struct kobj_attribute lwmi_scalar_increment = __ATTR_RO(scalar_increment);
static struct kobj_attribute lwmi_type = __ATTR_RO(type);

static struct attribute *lwmi_tunable_attrs[] = {
	&lwmi_current_value.attr,
	&lwmi_default_value.attr,
	&lwmi_display_name.attr,
	&lwmi_max_value.attr,
	&lwmi_min_value.attr,
	&lwmi_scalar_increment.attr,
	&lwmi_type.attr,
	NULL,
};

static const struct attribute_group lwmi_tunable_group = {
	.attrs = lwmi_tunable_attrs,
};

/*
 * [UP] wmi-other.c lwmi_attr_01_is_supported(): a supported record in custom
 * mode or in no mode, and a current value in one of those modes.
 */
static bool lwmi_tunable_supported(struct lwmi_priv *priv, struct lwmi_tunable *t)
{
	static const u8 modes[] = { LWMI_MODE_CUSTOM, LWMI_MODE_NONE };
	struct lwmi_capdata01 cd;
	bool found = false;
	u32 value;
	int i;

	for (i = 0; i < ARRAY_SIZE(modes) && !found; i++) {
		found = !lwmi_capdata01(priv, lwmi_tunable_id(t, modes[i]), &cd) &&
			le32_to_cpu(cd.supported);
		t->cd_mode = modes[i];
	}
	if (!found)
		return false;

	for (i = 0; i < ARRAY_SIZE(modes); i++) {
		if (!lwmi_feature_get(priv, lwmi_tunable_id(t, modes[i]), &value) && value) {
			t->cv_mode = modes[i];
			return true;
		}
	}
	return false;
}

static void lwmi_fw_attr_remove(void *data)
{
	struct lwmi_priv *priv = data;
	int i;

	for (i = 0; i < ARRAY_SIZE(lwmi_tunables); i++) {
		kobject_put(lwmi_tunables[i].kobj);
		lwmi_tunables[i].kobj = NULL;
	}
	kset_unregister(priv->fw_kset);
	device_unregister(priv->fw_dev);
}

static int lwmi_fw_attr_add(struct lwmi_priv *priv)
{
	struct lwmi_tunable *t;
	int ret, i;

	/* the name of [UP] lenovo-wmi-other, so userspace finds the same device */
	priv->fw_dev = device_create(&firmware_attributes_class, NULL, MKDEV(0, 0), priv,
				     "lenovo-wmi-other-0");
	if (IS_ERR(priv->fw_dev))
		return PTR_ERR(priv->fw_dev);

	priv->fw_kset = kset_create_and_add("attributes", NULL, &priv->fw_dev->kobj);
	if (!priv->fw_kset) {
		device_unregister(priv->fw_dev);
		return -ENOMEM;
	}

	ret = devm_add_action_or_reset(&priv->gz->dev, lwmi_fw_attr_remove, priv);
	if (ret)
		return ret;

	for (i = 0; i < ARRAY_SIZE(lwmi_tunables); i++) {
		t = &lwmi_tunables[i];
		if (!lwmi_tunable_supported(priv, t))
			continue;

		t->kobj = kobject_create_and_add(t->name, &priv->fw_kset->kobj);
		if (!t->kobj)
			return -ENOMEM;

		ret = sysfs_create_group(t->kobj, &lwmi_tunable_group);
		if (ret)
			return ret;
	}
	return 0;
}

/* ======== Platform profile (GameZone) ======== */

static int lwmi_profile_probe(void *drvdata, unsigned long *choices)
{
	struct lwmi_priv *priv = drvdata;
	u32 version;
	int ret;

	ret = lwmi_call_u32(priv->gz, LWMI_GZ_SMARTFAN_SUP, NULL, 0, &version);
	if (ret)
		return ret;
	if (version < 1)
		return -ENODEV;

	set_bit(PLATFORM_PROFILE_LOW_POWER, choices);
	set_bit(PLATFORM_PROFILE_BALANCED, choices);
	set_bit(PLATFORM_PROFILE_PERFORMANCE, choices);
	set_bit(PLATFORM_PROFILE_CUSTOM, choices);
	/* [UP] wmi-gamezone.c: the extreme mode exists from version 6 */
	if (version >= 6)
		set_bit(PLATFORM_PROFILE_MAX_POWER, choices);
	return 0;
}

static int lwmi_profile_get(struct device *dev, enum platform_profile_option *profile)
{
	struct lwmi_priv *priv = dev_get_drvdata(dev);
	u32 mode;
	int ret;

	ret = lwmi_mode(priv, &mode);
	if (ret)
		return ret;

	switch (mode) {
	case LWMI_MODE_QUIET:
		*profile = PLATFORM_PROFILE_LOW_POWER;
		return 0;
	case LWMI_MODE_BALANCED:
		*profile = PLATFORM_PROFILE_BALANCED;
		return 0;
	case LWMI_MODE_PERFORMANCE:
		*profile = PLATFORM_PROFILE_PERFORMANCE;
		return 0;
	case LWMI_MODE_EXTREME:
		*profile = PLATFORM_PROFILE_MAX_POWER;
		return 0;
	case LWMI_MODE_CUSTOM:
		*profile = PLATFORM_PROFILE_CUSTOM;
		return 0;
	default:
		return -EINVAL;
	}
}

static int lwmi_profile_set(struct device *dev, enum platform_profile_option profile)
{
	struct lwmi_priv *priv = dev_get_drvdata(dev);
	__le32 args[2] = {};
	int ret;

	switch (profile) {
	case PLATFORM_PROFILE_LOW_POWER:
		args[0] = cpu_to_le32(LWMI_MODE_QUIET);
		break;
	case PLATFORM_PROFILE_BALANCED:
		args[0] = cpu_to_le32(LWMI_MODE_BALANCED);
		break;
	case PLATFORM_PROFILE_PERFORMANCE:
		args[0] = cpu_to_le32(LWMI_MODE_PERFORMANCE);
		break;
	case PLATFORM_PROFILE_MAX_POWER:
		args[0] = cpu_to_le32(LWMI_MODE_EXTREME);
		break;
	case PLATFORM_PROFILE_CUSTOM:
		args[0] = cpu_to_le32(LWMI_MODE_CUSTOM);
		break;
	default:
		return -EOPNOTSUPP;
	}

	scoped_guard(mutex, &priv->lock)
		ret = lwmi_call(priv->gz, LWMI_GZ_SMARTFAN_SET, args, sizeof(args), NULL, 0);
	if (ret)
		return ret;

	lwmi_reassert(priv, false);
	return 0;
}

static const struct platform_profile_ops lwmi_profile_ops = {
	.probe = lwmi_profile_probe,
	.profile_get = lwmi_profile_get,
	.profile_set = lwmi_profile_set,
};

/* Fn+Q: tell userspace, then set again what custom mode needs */
static void lwmi_event_work(struct work_struct *work)
{
	struct lwmi_priv *priv = container_of(work, struct lwmi_priv, event_work);

	platform_profile_notify(priv->ppdev);
	lwmi_reassert(priv, false);
}

static void lwmi_cancel_event_work(void *data)
{
	struct lwmi_priv *priv = data;

	cancel_work_sync(&priv->event_work);
}

/* ======== Hub: the GameZone device ======== */

static int lwmi_link(struct lwmi_priv *priv, enum lwmi_part part, struct wmi_device **wdev)
{
	*wdev = lwmi_parts[part];
	if (!*wdev)
		return -EPROBE_DEFER;

	if (!device_link_add(&priv->gz->dev, &(*wdev)->dev, DL_FLAG_AUTOREMOVE_CONSUMER))
		return -EINVAL;
	return 0;
}

static int lwmi_gz_probe(struct wmi_device *wdev, const void *context)
{
	struct device *dev = &wdev->dev;
	struct lwmi_priv *priv;
	struct device *hwmon;
	int ret;

	priv = devm_kzalloc(dev, sizeof(*priv), GFP_KERNEL);
	if (!priv)
		return -ENOMEM;

	priv->gz = wdev;
	dev_set_drvdata(dev, priv);
	INIT_WORK(&priv->event_work, lwmi_event_work);
	ret = devm_mutex_init(dev, &priv->lock);
	if (ret)
		return ret;

	scoped_guard(mutex, &lwmi_registry_lock) {
		ret = lwmi_link(priv, LWMI_OTHER, &priv->other) ?:
		      lwmi_link(priv, LWMI_CAPDATA01, &priv->cd01) ?:
		      lwmi_link(priv, LWMI_FAN, &priv->fan);
	}
	if (ret)
		return ret;

	priv->ppdev = devm_platform_profile_register(dev, "lenovo-wmi-gamezone", priv,
						     &lwmi_profile_ops);
	if (IS_ERR(priv->ppdev))
		return PTR_ERR(priv->ppdev);

	ret = devm_add_action_or_reset(dev, lwmi_cancel_event_work, priv);
	if (ret)
		return ret;

	ret = lwmi_fw_attr_add(priv);
	if (ret)
		return ret;

	hwmon = devm_hwmon_device_register_with_info(dev, "legionctl_wmi", priv,
						     &lwmi_hwmon_chip_info, lwmi_fan_groups);
	if (IS_ERR(hwmon))
		return PTR_ERR(hwmon);

	scoped_guard(mutex, &lwmi_registry_lock)
		lwmi_hub = priv;
	return 0;
}

static void lwmi_gz_remove(struct wmi_device *wdev)
{
	guard(mutex)(&lwmi_registry_lock);
	lwmi_hub = NULL;
}

static int lwmi_gz_resume(struct device *dev)
{
	lwmi_reassert(dev_get_drvdata(dev), true);
	return 0;
}

static DEFINE_SIMPLE_DEV_PM_OPS(lwmi_gz_pm, NULL, lwmi_gz_resume);

static const struct wmi_device_id lwmi_gz_id_table[] = {
	{ LWMI_GAMEZONE_GUID, NULL },
	{}
};
MODULE_DEVICE_TABLE(wmi, lwmi_gz_id_table);

static struct wmi_driver lwmi_gz_driver = {
	.driver = {
		.name = "legionctl_wmi_gamezone",
		.pm = pm_sleep_ptr(&lwmi_gz_pm),
	},
	.id_table = lwmi_gz_id_table,
	.probe = lwmi_gz_probe,
	.remove = lwmi_gz_remove,
};

/* ======== Parts: Other Mode, Capability Data 01, fan method, events, LEDs ======== */

struct lwmi_leds {
	struct wmi_device *wdev;
	struct led_classdev micmute;
	struct led_classdev mute;
};

static int lwmi_led_set(struct wmi_device *wdev, u8 id)
{
	/* [UP] wmi-hotkey-utilities.c struct wmi_led_args: id, subid, value */
	u8 args[4] = { id };

	return lwmi_call(wdev, LWMI_LUD_SET_FEATURE, args, sizeof(args), NULL, 0);
}

static int lwmi_micmute_set(struct led_classdev *cdev, enum led_brightness b)
{
	struct lwmi_leds *leds = container_of(cdev, struct lwmi_leds, micmute);

	return lwmi_led_set(leds->wdev, b ? LWMI_LUD_MICMUTE_ON : LWMI_LUD_MICMUTE_OFF);
}

static int lwmi_mute_set(struct led_classdev *cdev, enum led_brightness b)
{
	struct lwmi_leds *leds = container_of(cdev, struct lwmi_leds, mute);

	return lwmi_led_set(leds->wdev, b ? LWMI_LUD_AUDIOMUTE_ON : LWMI_LUD_AUDIOMUTE_OFF);
}

static int lwmi_led_add(struct lwmi_leds *leds, struct led_classdev *cdev, u32 query,
			u32 supported, const char *name, const char *trigger,
			int (*set)(struct led_classdev *, enum led_brightness))
{
	__le32 arg = cpu_to_le32(query);
	u32 version;
	int ret;

	ret = lwmi_call_u32(leds->wdev, LWMI_LUD_GET_SUPPORT, &arg, sizeof(arg), &version);
	if (ret)
		return ret;
	/* [UP] 0: no LED; another version than the known one is not supported */
	if (version != supported)
		return 0;

	cdev->name = name;
	cdev->default_trigger = trigger;
	cdev->brightness_set_blocking = set;
	cdev->max_brightness = LED_ON;
	cdev->flags = LED_CORE_SUSPENDRESUME;
	return devm_led_classdev_register(&leds->wdev->dev, cdev);
}

static int lwmi_leds_probe(struct wmi_device *wdev)
{
	struct lwmi_leds *leds;

	leds = devm_kzalloc(&wdev->dev, sizeof(*leds), GFP_KERNEL);
	if (!leds)
		return -ENOMEM;

	leds->wdev = wdev;
	return lwmi_led_add(leds, &leds->micmute, LWMI_LUD_MICMUTE_VER,
			    LWMI_LUD_MICMUTE_SUPPORTED, "platform::micmute", "audio-micmute",
			    lwmi_micmute_set) ?:
	       lwmi_led_add(leds, &leds->mute, LWMI_LUD_AUDIOMUTE_VER,
			    LWMI_LUD_AUDIOMUTE_SUPPORTED, "platform::mute", "audio-mute",
			    lwmi_mute_set);
}

static int lwmi_part_probe(struct wmi_device *wdev, const void *context)
{
	const enum lwmi_part *part = context;
	int ret = 0;

	if (*part == LWMI_UTILITY)
		ret = lwmi_leds_probe(wdev);
	if (ret)
		return ret;

	guard(mutex)(&lwmi_registry_lock);
	lwmi_parts[*part] = wdev;
	return 0;
}

static void lwmi_part_remove(struct wmi_device *wdev)
{
	int i;

	guard(mutex)(&lwmi_registry_lock);
	for (i = 0; i < LWMI_PARTS; i++)
		if (lwmi_parts[i] == wdev)
			lwmi_parts[i] = NULL;
}

/* [UP] wmi-events.c: the thermal mode event carries the new mode */
static void lwmi_part_notify(struct wmi_device *wdev, const struct wmi_buffer *data)
{
	u32 mode;

	if (data->length < sizeof(u32))
		return;

	mode = get_unaligned_le32(data->data);
	if (!strcmp(lwmi_mode_name(mode), "unknown")) {
		dev_warn(&wdev->dev, "unknown thermal mode %#x in the event\n", mode);
		return;
	}

	guard(mutex)(&lwmi_registry_lock);
	if (lwmi_hub)
		schedule_work(&lwmi_hub->event_work);
}

static const enum lwmi_part lwmi_part_ids[] = {
	LWMI_OTHER, LWMI_CAPDATA01, LWMI_FAN, LWMI_EVENT, LWMI_UTILITY,
};

static const struct wmi_device_id lwmi_part_id_table[] = {
	{ LWMI_OTHER_GUID, &lwmi_part_ids[LWMI_OTHER] },
	{ LWMI_CAPDATA01_GUID, &lwmi_part_ids[LWMI_CAPDATA01] },
	{ LWMI_FAN_GUID, &lwmi_part_ids[LWMI_FAN] },
	{ LWMI_EVENT_GUID, &lwmi_part_ids[LWMI_EVENT] },
	{ LWMI_UTILITY_GUID, &lwmi_part_ids[LWMI_UTILITY] },
	{}
};
MODULE_DEVICE_TABLE(wmi, lwmi_part_id_table);

static struct wmi_driver lwmi_part_driver = {
	.driver = {
		.name = "legionctl_wmi",
	},
	.id_table = lwmi_part_id_table,
	.probe = lwmi_part_probe,
	.remove = lwmi_part_remove,
	.notify_new = lwmi_part_notify,
};

static int __init lwmi_init(void)
{
	int ret;

	ret = wmi_driver_register(&lwmi_part_driver);
	if (ret)
		return ret;

	ret = wmi_driver_register(&lwmi_gz_driver);
	if (ret)
		wmi_driver_unregister(&lwmi_part_driver);
	return ret;
}
module_init(lwmi_init);

static void __exit lwmi_exit(void)
{
	wmi_driver_unregister(&lwmi_gz_driver);
	wmi_driver_unregister(&lwmi_part_driver);
}
module_exit(lwmi_exit);

MODULE_DESCRIPTION("Lenovo Legion Pro 7 16IRX8H WMI driver (legionctl)");
MODULE_LICENSE("GPL");

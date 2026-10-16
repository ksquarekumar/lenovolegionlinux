// SPDX-License-Identifier: GPL-2.0
/*
 * Test helper of the legionctl-wmi test (guest only): evaluate one ACPI method of
 * the stand-in firmware when loaded, as the EC does for Fn+Q. Use:
 * insmod aml_call.ko method='\_SB.GZFD.FNQ' && rmmod aml_call
 */

#include <linux/acpi.h>
#include <linux/module.h>

static char *method;
module_param(method, charp, 0444);

static int __init aml_call_init(void)
{
	acpi_status status = acpi_evaluate_object(NULL, method, NULL, NULL);

	return ACPI_FAILURE(status) ? -EIO : 0;
}
module_init(aml_call_init);

static void __exit aml_call_exit(void)
{
}
module_exit(aml_call_exit);

MODULE_DESCRIPTION("Evaluate one ACPI method (legionctl-wmi test helper)");
MODULE_LICENSE("GPL");

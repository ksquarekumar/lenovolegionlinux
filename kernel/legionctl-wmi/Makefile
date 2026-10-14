# SPDX-License-Identifier: GPL-2.0
# Build against the headers of a kernel: make KDIR=/usr/lib/modules/<version>/build
KDIR ?= /usr/lib/modules/$(shell uname -r)/build
# a kernel built with clang needs the LLVM toolchain for its modules
LLVM_FLAG := $(if $(shell grep -s '^CONFIG_CC_IS_CLANG=y' $(KDIR)/.config),LLVM=1)

.PHONY: modules clean
modules:
	$(MAKE) -C $(KDIR) M=$(CURDIR) $(LLVM_FLAG) W=1 modules

clean:
	$(MAKE) -C $(KDIR) M=$(CURDIR) $(LLVM_FLAG) clean

// SPDX-License-Identifier: GPL-3.0-or-later
// Lets a module-supplied ca.pem replace the system trust store, which is how
// users add a private CA on devices where /system is read-only.
package main

import (
	"crypto/x509"
	"log"
	_ "unsafe" // for go:linkname
)

//go:linkname systemRoots crypto/x509.systemRoots
var systemRoots *x509.CertPool

func updateRootCACerts(pem []byte) {
	x509.SystemCertPool()
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(pem) {
		log.Println("failed to append certificates from ca.pem")
		return
	}
	systemRoots = roots
	log.Println("ca.pem loaded from module assets")
}

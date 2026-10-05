package io.github.rsyumi.risunest

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Test
import org.w3c.dom.Document
import org.w3c.dom.Element

class NetworkSecurityConfigTest {
  private val android = "http://schemas.android.com/apk/res/android"

  private fun parse(path: String): Document =
    DocumentBuilderFactory.newInstance().apply { isNamespaceAware = true }
      .newDocumentBuilder().parse(File(path))

  private fun Document.elements(tag: String): List<Element> {
    val nodes = getElementsByTagName(tag)
    return (0 until nodes.length).map { nodes.item(it) as Element }
  }

  @Test
  fun `the app applies its network security config`() {
    val application = parse("src/main/AndroidManifest.xml").elements("application").single()

    assertEquals("@xml/network_security_config", application.getAttributeNS(android, "networkSecurityConfig"))
    // The config replaces this attribute on every supported API level.
    assertEquals("", application.getAttributeNS(android, "usesCleartextTraffic"))
  }

  @Test
  fun `every connection may use HTTP and trusts system and user CAs`() {
    val config = parse("src/main/res/xml/network_security_config.xml")
    val base = config.elements("base-config").single()

    assertEquals("true", base.getAttribute("cleartextTrafficPermitted"))
    assertEquals(
      listOf("system", "user"),
      config.elements("certificates").map { it.getAttribute("src") },
    )
    assertEquals(emptyList<Element>(), config.elements("domain-config"))
    assertEquals(emptyList<Element>(), config.elements("debug-overrides"))
  }
}

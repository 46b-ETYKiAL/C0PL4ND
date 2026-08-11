<?xml version="1.0" encoding="utf-8"?>
<!--
  heat(1) transform: give every harvested license directory a RemoveFolder row.

  `heat dir` emits Directory/Component/File elements but NEVER a RemoveFolder,
  so a harvested tree leaves its directories behind on uninstall. C0PL4ND
  installs per-machine under Program Files, so ICE64 - which only polices
  user-profile directories - does not fail the link here. Without these rows an
  uninstall would still leave the entire licenses\fonts\* tree behind as empty
  directories under Program Files, which is what they fix.

  This file is deliberately IDENTICAL in behaviour to the SCR1B3 copy at
  crates/scribe-app/wix/license-harvest.xsl, where the same rows are
  load-bearing: SCR1B3 installs into %LOCALAPPDATA%\Programs\SCR1B3, so ICE64
  fails its link with one LGHT0204 per harvested directory unless they are
  authored. Suppressing that with -sice:ICE64 would "work" and would discard the
  check that catches the next uncleaned directory, so the rows are authored in
  both repos rather than the ICE being turned off in one.

  Removal is empty-only, so a directory holding anything the user left behind
  still survives (verified with a foreign-occupant install/uninstall probe).

  One RemoveFolder per (Component, ancestor directory) pair rather than per
  Directory: several Components can share a Directory, and duplicate rows for
  one directory are legal (each is keyed by its own Id). Deriving the Id from
  the Component Id keeps it unique and stable across builds.
-->
<xsl:stylesheet version="1.0"
                xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
                xmlns:wix="http://schemas.microsoft.com/wix/2006/wi"
                xmlns="http://schemas.microsoft.com/wix/2006/wi"
                exclude-result-prefixes="wix">

  <xsl:output method="xml" indent="yes" />

  <!-- Identity: copy everything heat produced, unchanged. -->
  <xsl:template match="@*|node()">
    <xsl:copy>
      <xsl:apply-templates select="@*|node()" />
    </xsl:copy>
  </xsl:template>

  <!--
    Only Components nested in a harvested <Directory>. Components sitting
    directly under the <DirectoryRef> are in INSTALLFOLDER, which the installer
    removes along with the product itself.

    Every ANCESTOR Directory is covered, not just the immediate parent. An
    intermediate directory can hold no files of its own - `licenses/fonts`
    contains only the 22 per-font subdirectories - so it gets no Component, and
    a parent-only rule leaves exactly that one directory unlisted. Measured, not
    predicted: on SCR1B3 the parent-only version of this transform emitted rows
    for 24 of 25 directories and `light` still failed with one ICE64, for
    `licenses/fonts`.
  -->
  <xsl:template match="wix:Component[parent::wix:Directory]">
    <xsl:variable name="cmp" select="substring(@Id, 4)" />
    <xsl:copy>
      <xsl:apply-templates select="@*|node()" />
      <xsl:for-each select="ancestor::wix:Directory">
        <RemoveFolder On="uninstall">
          <!-- heat Ids are `cmp<32 hex>`; `rmf<32 hex>_<n>` is unique per
               (component, ancestor) pair and well under the 72-char
               Identifier limit. -->
          <xsl:attribute name="Id">
            <xsl:value-of select="concat('rmf', $cmp, '_', position())" />
          </xsl:attribute>
          <xsl:attribute name="Directory">
            <xsl:value-of select="@Id" />
          </xsl:attribute>
        </RemoveFolder>
      </xsl:for-each>
    </xsl:copy>
  </xsl:template>

</xsl:stylesheet>

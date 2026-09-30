class MExtAgreement(agreement: String, ext: String)(implicit queryService: Int)
  extends MAgreement(agreement)(queryService) with ExtAgreement with OtherAgreement {
  def render(): String = extId
}

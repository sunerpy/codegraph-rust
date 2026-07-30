object DeepForm: TDeepForm
  Caption = 'Deep'
  OnCreate = DeepFormCreate
  object TopPanel: TPanel
    Caption = 'Top'
    object InnerPanel: TPanel
      Caption = 'Inner'
      object DeepButton: TButton
        Caption = 'Deep'
        OnClick = DeepButtonClick
      end
      object DeepLabel: TLabel
        Caption = 'Label'
      end
    end
    object SiblingButton: TButton
      Caption = 'Sibling'
    end
  end
  object BottomPanel: TPanel
    Caption = 'Bottom'
    object Items: TListView
      Columns = <
        item
          Caption = 'Name'
        end
        item
          Caption = 'Size'
        end>
      OnDblClick = ItemsDblClick
    end
  end
end

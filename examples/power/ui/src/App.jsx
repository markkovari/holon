import { useState } from 'react'

function App() {
  const [wattage, setWattage] = useState(0)
  const [hours, setHours] = useState(0)
  const [cost, setCost] = useState(0)

  const handleCalculate = () => {
    const kwh = (wattage * hours) / 1000;
    setCost(kwh * 0.15);
  }

  return (
    <div>
      <h1>Power Consumption Calculator</h1>
      <input 
        type="number" 
        placeholder="Wattage" 
        value={wattage} 
        onChange={(e) => setWattage(e.target.value)} 
        data-testid="wattage-input"
      />
      <input 
        type="number" 
        placeholder="Hours" 
        value={hours} 
        onChange={(e) => setHours(e.target.value)} 
        data-testid="hours-input"
      />
      <button onClick={handleCalculate} data-testid="calculate-button">Calculate</button>
      <p data-testid="cost-result">Cost: ${cost.toFixed(2)}</p>
    </div>
  )
}

export default App
